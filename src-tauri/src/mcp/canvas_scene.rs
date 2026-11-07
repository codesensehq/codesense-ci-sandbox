// SPDX-License-Identifier: MIT
//! Validated canvas writer — skeleton → Excalidraw scene expansion.
//!
//! Pure, IO-free. The `apply_canvas` MCP verb (`handler.rs`) feeds agent-
//! supplied *skeleton* elements (`{type, x, y, width?, ...}`) through
//! [`build_scene`], which validates them and expands each into a full
//! Excalidraw element, then serializes the `.excalidraw` envelope. The verb
//! writes the result via `write_content_file` (the sole writer). Nothing here
//! touches disk or the frontend — it is a Rust action, headless-capable.
//!
//! **Why this isn't a faithful `convertToExcalidrawElements` clone:** the
//! viewer mounts scenes via `<Excalidraw initialData>`, and Excalidraw runs
//! `restore` on load — it normalizes/repairs elements and *drops unknown
//! types*. So the bar here is "emit elements `restore` keeps" (valid `type` +
//! finite geometry + the required field set), not byte-parity with Excalidraw
//! internals. `restore` re-syncs fractional indices, recomputes text metrics
//! on edit, and fills any minor gaps. The viewer's read-side guard is the
//! final backstop.

use serde::Deserialize;
use serde_json::{json, Map, Value};

/// Excalidraw scene-file format version (the `version` field of the envelope,
/// distinct from per-element `version`). Pinned to the `@excalidraw/excalidraw`
/// major the app bundles.
const SCENE_VERSION: u32 = 2;
const SCENE_TYPE: &str = "excalidraw";
const SOURCE: &str = "codesense-mcp:apply_canvas";

/// Element types this verb can author. Anything outside this set is rejected
/// at the boundary (and would be dropped by Excalidraw `restore` anyway).
const SUPPORTED_TYPES: &[&str] = &["rectangle", "ellipse", "diamond", "text", "line", "arrow"];

// --- Excalidraw element defaults (mirror `@excalidraw/excalidraw` constants;
//     restore tolerates these verbatim). ---
const DEFAULT_STROKE_COLOR: &str = "#1e1e1e";
const DEFAULT_BG_COLOR: &str = "transparent";
const DEFAULT_FILL_STYLE: &str = "solid";
const DEFAULT_STROKE_WIDTH: f64 = 2.0;
const DEFAULT_STROKE_STYLE: &str = "solid";
const DEFAULT_ROUGHNESS: f64 = 1.0;
const DEFAULT_OPACITY: f64 = 100.0;
const DEFAULT_FONT_SIZE: f64 = 20.0;
/// Excalidraw default font family (5 = the hand-drawn "Excalifont" in 0.18).
const DEFAULT_FONT_FAMILY: u32 = 5;
const DEFAULT_LINE_HEIGHT: f64 = 1.25;
const DEFAULT_SHAPE_W: f64 = 100.0;
const DEFAULT_SHAPE_H: f64 = 100.0;

/// Agent-supplied lightweight element. Only `type`, `x`, `y` are universally
/// required; the rest are type-specific and defaulted during expansion.
#[derive(Debug, Clone, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CanvasSkeleton {
    /// One of: rectangle, ellipse, diamond, text, line, arrow.
    #[serde(rename = "type")]
    pub kind: String,
    pub x: f64,
    pub y: f64,
    /// Shape width. Defaults to 100 for shapes; derived from `points` for lines.
    #[serde(default)]
    pub width: Option<f64>,
    #[serde(default)]
    pub height: Option<f64>,
    /// Required for `type: "text"`: the text content.
    #[serde(default)]
    pub text: Option<String>,
    /// Optional bound text label for a container shape (rectangle/ellipse/diamond).
    #[serde(default)]
    pub label: Option<CanvasLabel>,
    /// For `line`/`arrow`: relative points `[[0,0],[dx,dy],...]`. Defaults to a
    /// straight segment derived from width/height when omitted.
    #[serde(default)]
    pub points: Option<Vec<[f64; 2]>>,
    #[serde(default)]
    pub stroke_color: Option<String>,
    #[serde(default)]
    pub background_color: Option<String>,
}

/// Bound text label for a container shape.
#[derive(Debug, Clone, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CanvasLabel {
    pub text: String,
    #[serde(default)]
    pub font_size: Option<f64>,
}

/// Expand + validate `skeletons` into a serialized `.excalidraw` scene string.
/// Uses a non-deterministic seed for per-element `seed`/`versionNonce`.
pub fn build_scene(skeletons: &[CanvasSkeleton]) -> Result<String, String> {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    build_scene_with_seed(skeletons, seed)
}

/// Seed-injected variant for deterministic tests.
pub fn build_scene_with_seed(
    skeletons: &[CanvasSkeleton],
    rng_seed: u64,
) -> Result<String, String> {
    if skeletons.is_empty() {
        return Err("apply_canvas requires at least one element".to_string());
    }
    let mut rng = Lcg::new(rng_seed);
    let mut elements: Vec<Value> = Vec::with_capacity(skeletons.len());

    for (i, sk) in skeletons.iter().enumerate() {
        expand_element(sk, &mut rng, &mut elements)
            .map_err(|e| format!("element[{i}] ({}): {e}", sk.kind))?;
    }

    // Assign fractional indices by *final array position* — one source of truth
    // for z-order. This gives every element (including a container's bound text,
    // which is emitted in the same step) a unique, strictly-increasing index, so
    // we never lean on Excalidraw `restore` to de-duplicate.
    for (i, el) in elements.iter_mut().enumerate() {
        if let Some(obj) = el.as_object_mut() {
            obj.insert("index".into(), json!(fractional_index(i)));
        }
    }

    let scene = json!({
        "type": SCENE_TYPE,
        "version": SCENE_VERSION,
        "source": SOURCE,
        "elements": elements,
        "appState": {},
        "files": {},
    });
    serde_json::to_string_pretty(&scene).map_err(|e| format!("scene serialization failed: {e}"))
}

/// Expand one skeleton, pushing the resulting element(s) onto `out`. A
/// container with a `label` pushes both the container and its bound text.
fn expand_element(sk: &CanvasSkeleton, rng: &mut Lcg, out: &mut Vec<Value>) -> Result<(), String> {
    if !SUPPORTED_TYPES.contains(&sk.kind.as_str()) {
        return Err(format!(
            "unsupported type (must be one of {})",
            SUPPORTED_TYPES.join(", ")
        ));
    }
    if !sk.x.is_finite() || !sk.y.is_finite() {
        return Err("x and y must be finite numbers".to_string());
    }
    if let Some(w) = sk.width {
        if !w.is_finite() || w < 0.0 {
            return Err("width must be a finite, non-negative number".to_string());
        }
    }
    if let Some(h) = sk.height {
        if !h.is_finite() || h < 0.0 {
            return Err("height must be a finite, non-negative number".to_string());
        }
    }

    match sk.kind.as_str() {
        "text" => {
            // Reject blank text: an empty/whitespace-only string survives the
            // write but Excalidraw `restore` strips it, leaving a phantom gap
            // in the rendered scene.
            let text = sk
                .text
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .ok_or("type 'text' requires a non-blank `text`")?;
            out.push(text_element(sk, text, rng, None));
        }
        "line" | "arrow" => {
            out.push(linear_element(sk, rng)?);
        }
        _ => {
            // rectangle | ellipse | diamond — a container, optionally labeled.
            // Reject zero-area shapes: a 0-width/height shape renders as an
            // invisible point. (Defaults are non-zero; only an explicit 0 trips.)
            let width = sk.width.unwrap_or(DEFAULT_SHAPE_W);
            let height = sk.height.unwrap_or(DEFAULT_SHAPE_H);
            if width <= 0.0 || height <= 0.0 {
                return Err("shape width and height must be greater than zero".to_string());
            }
            let container_id = gen_id(rng);
            let mut container = base_element(&container_id, &sk.kind, rng);
            let obj = container.as_object_mut().unwrap();
            obj.insert("x".into(), json!(sk.x));
            obj.insert("y".into(), json!(sk.y));
            obj.insert("width".into(), json!(width));
            obj.insert("height".into(), json!(height));
            apply_style_overrides(obj, sk);

            if let Some(label) = &sk.label {
                // Same non-blank guarantee as a standalone text element, plus a
                // finite/positive font size — otherwise the bound text is a
                // dangling `boundElements` reference once `restore` drops it.
                if label.text.trim().is_empty() {
                    return Err("container `label.text` must not be blank".to_string());
                }
                if let Some(fs) = label.font_size {
                    if !fs.is_finite() || fs <= 0.0 {
                        return Err(
                            "`label.fontSize` must be a finite, positive number".to_string()
                        );
                    }
                }
                let text_id = gen_id(rng);
                obj.insert(
                    "boundElements".into(),
                    json!([{ "id": text_id, "type": "text" }]),
                );
                // Bound text: a centered text element pointing back via containerId.
                let label_sk = CanvasSkeleton {
                    kind: "text".into(),
                    x: sk.x,
                    y: sk.y,
                    width: None,
                    height: None,
                    text: Some(label.text.clone()),
                    label: None,
                    points: None,
                    stroke_color: None,
                    background_color: None,
                };
                let mut text_el = text_element(&label_sk, &label.text, rng, Some(&container_id));
                let tobj = text_el.as_object_mut().unwrap();
                tobj.insert("id".into(), json!(text_id));
                tobj.insert("textAlign".into(), json!("center"));
                tobj.insert("verticalAlign".into(), json!("middle"));
                if let Some(fs) = label.font_size {
                    tobj.insert("fontSize".into(), json!(fs));
                }
                out.push(container);
                out.push(text_el);
            } else {
                out.push(container);
            }
        }
    }
    Ok(())
}

/// The shared `_ExcalidrawElementBase` field set, with sensible defaults. The
/// caller augments per-type (width/height, points, text fields, ...); `index`
/// is assigned by `build_scene_with_seed` once final z-order is known.
fn base_element(id: &str, kind: &str, rng: &mut Lcg) -> Value {
    json!({
        "id": id,
        "type": kind,
        "x": 0.0,
        "y": 0.0,
        "width": 0.0,
        "height": 0.0,
        "angle": 0.0,
        "strokeColor": DEFAULT_STROKE_COLOR,
        "backgroundColor": DEFAULT_BG_COLOR,
        "fillStyle": DEFAULT_FILL_STYLE,
        "strokeWidth": DEFAULT_STROKE_WIDTH,
        "strokeStyle": DEFAULT_STROKE_STYLE,
        "roughness": DEFAULT_ROUGHNESS,
        "opacity": DEFAULT_OPACITY,
        "roundness": Value::Null,
        "seed": rng.next_i32(),
        "version": 1,
        "versionNonce": rng.next_i32(),
        "index": Value::Null,
        "isDeleted": false,
        "groupIds": [],
        "frameId": Value::Null,
        "boundElements": Value::Null,
        "updated": 1,
        "link": Value::Null,
        "locked": false,
    })
}

fn text_element(
    sk: &CanvasSkeleton,
    text: &str,
    rng: &mut Lcg,
    container_id: Option<&str>,
) -> Value {
    let id = gen_id(rng);
    let mut el = base_element(&id, "text", rng);
    let obj = el.as_object_mut().unwrap();
    let font_size = DEFAULT_FONT_SIZE;
    let line_count = text.lines().count().max(1) as f64;
    let longest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f64;
    // Approximate metrics; Excalidraw recomputes on edit.
    let width = sk
        .width
        .unwrap_or((longest * font_size * 0.6).max(font_size));
    let height = sk
        .height
        .unwrap_or(font_size * DEFAULT_LINE_HEIGHT * line_count);
    obj.insert("x".into(), json!(sk.x));
    obj.insert("y".into(), json!(sk.y));
    obj.insert("width".into(), json!(width));
    obj.insert("height".into(), json!(height));
    obj.insert("text".into(), json!(text));
    obj.insert("originalText".into(), json!(text));
    obj.insert("fontSize".into(), json!(font_size));
    obj.insert("fontFamily".into(), json!(DEFAULT_FONT_FAMILY));
    obj.insert("textAlign".into(), json!("left"));
    obj.insert("verticalAlign".into(), json!("top"));
    obj.insert("containerId".into(), json!(container_id));
    obj.insert("autoResize".into(), json!(true));
    obj.insert("lineHeight".into(), json!(DEFAULT_LINE_HEIGHT));
    apply_style_overrides(obj, sk);
    el
}

fn linear_element(sk: &CanvasSkeleton, rng: &mut Lcg) -> Result<Value, String> {
    let points: Vec<[f64; 2]> = match &sk.points {
        Some(p) => {
            if p.len() < 2 {
                return Err("line/arrow requires at least 2 points".to_string());
            }
            if p.iter().flatten().any(|v| !v.is_finite()) {
                return Err("line/arrow points must be finite numbers".to_string());
            }
            p.clone()
        }
        None => {
            // Derive a straight segment from width/height (defaulting to a
            // horizontal 100px segment).
            let dx = sk.width.unwrap_or(DEFAULT_SHAPE_W);
            let dy = sk.height.unwrap_or(0.0);
            vec![[0.0, 0.0], [dx, dy]]
        }
    };
    let (min_x, min_y, max_x, max_y) = points.iter().fold(
        (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
        |(nx, ny, xx, xy), p| (nx.min(p[0]), ny.min(p[1]), xx.max(p[0]), xy.max(p[1])),
    );
    // Reject a zero-length segment (all points coincide) — it renders as an
    // invisible point. A horizontal/vertical line (one extent zero) is fine.
    if (max_x - min_x).abs() == 0.0 && (max_y - min_y).abs() == 0.0 {
        return Err("line/arrow has zero length (all points coincide)".to_string());
    }
    let id = gen_id(rng);
    let mut el = base_element(&id, &sk.kind, rng);
    let obj = el.as_object_mut().unwrap();
    obj.insert("x".into(), json!(sk.x));
    obj.insert("y".into(), json!(sk.y));
    obj.insert("width".into(), json!((max_x - min_x).abs()));
    obj.insert("height".into(), json!((max_y - min_y).abs()));
    obj.insert(
        "points".into(),
        json!(points
            .iter()
            .map(|p| json!([p[0], p[1]]))
            .collect::<Vec<_>>()),
    );
    obj.insert("lastCommittedPoint".into(), Value::Null);
    obj.insert("startBinding".into(), Value::Null);
    obj.insert("endBinding".into(), Value::Null);
    obj.insert("startArrowhead".into(), Value::Null);
    obj.insert(
        "endArrowhead".into(),
        if sk.kind == "arrow" {
            json!("arrow")
        } else {
            Value::Null
        },
    );
    if sk.kind == "arrow" {
        obj.insert("elbowed".into(), json!(false));
    }
    apply_style_overrides(obj, sk);
    Ok(el)
}

fn apply_style_overrides(obj: &mut Map<String, Value>, sk: &CanvasSkeleton) {
    if let Some(c) = &sk.stroke_color {
        obj.insert("strokeColor".into(), json!(c));
    }
    if let Some(c) = &sk.background_color {
        obj.insert("backgroundColor".into(), json!(c));
    }
}

/// Fractional index for position `i` in the element array. Excalidraw re-syncs
/// these on load (`syncInvalidIndices`), so a simple monotonic scheme suffices.
fn fractional_index(i: usize) -> String {
    format!("a{i}")
}

/// Generate a unique element id from the RNG (base36, ~13 chars). Excalidraw
/// keeps any unique id; it need not match nanoid's format.
fn gen_id(rng: &mut Lcg) -> String {
    let mut n = rng.next_u64();
    let alphabet = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut s = String::with_capacity(13);
    for _ in 0..13 {
        s.push(alphabet[(n % 36) as usize] as char);
        n /= 36;
    }
    s
}

/// Tiny linear-congruential RNG — deterministic from a seed, no external dep.
/// Used only for element id/seed/versionNonce generation, not security.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        // Avoid a zero state (would stick the LCG); mix the seed.
        Lcg(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next_u64(&mut self) -> u64 {
        // Numerical Recipes LCG constants.
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn next_i32(&mut self) -> i32 {
        (self.next_u64() >> 33) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(scene: &str) -> Value {
        serde_json::from_str(scene).expect("scene must be valid JSON")
    }

    fn sk(kind: &str, x: f64, y: f64) -> CanvasSkeleton {
        CanvasSkeleton {
            kind: kind.into(),
            x,
            y,
            width: None,
            height: None,
            text: None,
            label: None,
            points: None,
            stroke_color: None,
            background_color: None,
        }
    }

    /// The base field set every element must carry for Excalidraw
    /// `restore` to keep it. Sourced from `_ExcalidrawElementBase`
    /// (`element/types.d.ts`, @excalidraw/excalidraw ^0.18.1).
    const REQUIRED_BASE_FIELDS: &[&str] = &[
        "id",
        "type",
        "x",
        "y",
        "width",
        "height",
        "angle",
        "strokeColor",
        "backgroundColor",
        "fillStyle",
        "strokeWidth",
        "strokeStyle",
        "roughness",
        "opacity",
        "roundness",
        "seed",
        "version",
        "versionNonce",
        "index",
        "isDeleted",
        "groupIds",
        "frameId",
        "boundElements",
        "updated",
        "link",
        "locked",
    ];

    fn assert_base_fields(el: &Value) {
        let obj = el.as_object().expect("element is an object");
        for f in REQUIRED_BASE_FIELDS {
            assert!(
                obj.contains_key(*f),
                "element missing base field `{f}`: {el}"
            );
        }
    }

    #[test]
    fn envelope_shape_is_excalidraw_v2() {
        let scene = parse(&build_scene_with_seed(&[sk("rectangle", 0.0, 0.0)], 1).unwrap());
        assert_eq!(scene["type"], "excalidraw");
        assert_eq!(scene["version"], SCENE_VERSION);
        assert!(scene["elements"].is_array());
        assert!(scene["appState"].is_object());
        assert!(scene["files"].is_object());
    }

    #[test]
    fn every_supported_shape_carries_required_base_fields() {
        for kind in ["rectangle", "ellipse", "diamond", "line", "arrow"] {
            let scene = parse(&build_scene_with_seed(&[sk(kind, 10.0, 20.0)], 7).unwrap());
            let els = scene["elements"].as_array().unwrap();
            assert_eq!(els.len(), 1, "{kind} should expand to one element");
            assert_eq!(els[0]["type"], kind);
            assert_base_fields(&els[0]);
        }
    }

    #[test]
    fn text_element_has_text_fields_and_required_base() {
        let mut s = sk("text", 5.0, 6.0);
        s.text = Some("hello\nworld".into());
        let scene = parse(&build_scene_with_seed(&[s], 3).unwrap());
        let el = &scene["elements"][0];
        assert_base_fields(el);
        assert_eq!(el["type"], "text");
        assert_eq!(el["text"], "hello\nworld");
        assert_eq!(el["originalText"], "hello\nworld");
        for f in [
            "fontSize",
            "fontFamily",
            "textAlign",
            "verticalAlign",
            "containerId",
            "autoResize",
            "lineHeight",
        ] {
            assert!(el.get(f).is_some(), "text missing `{f}`");
        }
        assert!(el["containerId"].is_null());
    }

    #[test]
    fn linear_element_has_points_and_arrow_head() {
        let arrow = parse(&build_scene_with_seed(&[sk("arrow", 0.0, 0.0)], 9).unwrap());
        let el = &arrow["elements"][0];
        assert_base_fields(el);
        assert!(el["points"].as_array().unwrap().len() >= 2);
        assert_eq!(el["endArrowhead"], "arrow");
        assert_eq!(el["elbowed"], false);
        assert!(el["startBinding"].is_null());

        let line = parse(&build_scene_with_seed(&[sk("line", 0.0, 0.0)], 9).unwrap());
        let lel = &line["elements"][0];
        assert!(lel["endArrowhead"].is_null());
        assert!(lel.get("elbowed").is_none(), "line is not an arrow");
    }

    #[test]
    fn container_with_label_wires_bound_text_both_ways() {
        let mut s = sk("rectangle", 0.0, 0.0);
        s.label = Some(CanvasLabel {
            text: "Save".into(),
            font_size: None,
        });
        let scene = parse(&build_scene_with_seed(&[s], 11).unwrap());
        let els = scene["elements"].as_array().unwrap();
        assert_eq!(els.len(), 2, "labeled container = container + text");
        let container = &els[0];
        let text = &els[1];
        let cid = container["id"].as_str().unwrap();
        let tid = text["id"].as_str().unwrap();
        assert_eq!(text["containerId"], cid);
        assert_eq!(container["boundElements"][0]["id"], tid);
        assert_eq!(container["boundElements"][0]["type"], "text");
        assert_eq!(text["text"], "Save");
        assert_eq!(text["textAlign"], "center");
        assert_eq!(text["verticalAlign"], "middle");
    }

    #[test]
    fn explicit_geometry_and_style_overrides_applied() {
        let mut s = sk("rectangle", 1.0, 2.0);
        s.width = Some(250.0);
        s.height = Some(80.0);
        s.stroke_color = Some("#ff0000".into());
        s.background_color = Some("#00ff00".into());
        let scene = parse(&build_scene_with_seed(&[s], 2).unwrap());
        let el = &scene["elements"][0];
        assert_eq!(el["x"], 1.0);
        assert_eq!(el["width"], 250.0);
        assert_eq!(el["height"], 80.0);
        assert_eq!(el["strokeColor"], "#ff0000");
        assert_eq!(el["backgroundColor"], "#00ff00");
    }

    #[test]
    fn indices_are_monotonic_across_elements() {
        let scene = parse(
            &build_scene_with_seed(
                &[
                    sk("rectangle", 0.0, 0.0),
                    sk("ellipse", 1.0, 1.0),
                    sk("diamond", 2.0, 2.0),
                ],
                5,
            )
            .unwrap(),
        );
        let els = scene["elements"].as_array().unwrap();
        assert_eq!(els[0]["index"], "a0");
        assert_eq!(els[1]["index"], "a1");
        assert_eq!(els[2]["index"], "a2");
    }

    // --- rejection cases (malformed never reaches disk) ---

    #[test]
    fn rejects_empty_element_list() {
        assert!(build_scene(&[]).is_err());
    }

    #[test]
    fn rejects_unknown_type() {
        let err = build_scene_with_seed(&[sk("hexagon", 0.0, 0.0)], 1).unwrap_err();
        assert!(err.contains("unsupported type"), "got: {err}");
    }

    #[test]
    fn rejects_non_finite_coords() {
        let err = build_scene_with_seed(&[sk("rectangle", f64::NAN, 0.0)], 1).unwrap_err();
        assert!(err.contains("finite"), "got: {err}");
    }

    #[test]
    fn rejects_text_without_text() {
        let err = build_scene_with_seed(&[sk("text", 0.0, 0.0)], 1).unwrap_err();
        assert!(err.contains("requires a non-blank `text`"), "got: {err}");
    }

    #[test]
    fn rejects_whitespace_only_text() {
        let mut s = sk("text", 0.0, 0.0);
        s.text = Some("   \n\t".into());
        let err = build_scene_with_seed(&[s], 1).unwrap_err();
        assert!(err.contains("non-blank"), "got: {err}");
    }

    #[test]
    fn rejects_blank_label_text() {
        let mut s = sk("rectangle", 0.0, 0.0);
        s.label = Some(CanvasLabel {
            text: "  ".into(),
            font_size: None,
        });
        let err = build_scene_with_seed(&[s], 1).unwrap_err();
        assert!(err.contains("`label.text` must not be blank"), "got: {err}");
    }

    #[test]
    fn rejects_non_positive_label_font_size() {
        let mut s = sk("rectangle", 0.0, 0.0);
        s.label = Some(CanvasLabel {
            text: "ok".into(),
            font_size: Some(-4.0),
        });
        let err = build_scene_with_seed(&[s], 1).unwrap_err();
        assert!(err.contains("label.fontSize"), "got: {err}");
    }

    #[test]
    fn rejects_zero_area_shape() {
        let mut s = sk("rectangle", 0.0, 0.0);
        s.width = Some(0.0);
        s.height = Some(50.0);
        let err = build_scene_with_seed(&[s], 1).unwrap_err();
        assert!(err.contains("greater than zero"), "got: {err}");
    }

    #[test]
    fn rejects_zero_length_arrow() {
        let mut s = sk("arrow", 5.0, 5.0);
        s.points = Some(vec![[0.0, 0.0], [0.0, 0.0]]);
        let err = build_scene_with_seed(&[s], 1).unwrap_err();
        assert!(err.contains("zero length"), "got: {err}");
    }

    #[test]
    fn horizontal_arrow_with_zero_height_is_allowed() {
        // One extent zero is a legitimate straight line — must NOT be rejected.
        let mut s = sk("arrow", 5.0, 5.0);
        s.points = Some(vec![[0.0, 0.0], [120.0, 0.0]]);
        assert!(build_scene_with_seed(&[s], 1).is_ok());
    }

    #[test]
    fn rejects_negative_width() {
        let mut s = sk("rectangle", 0.0, 0.0);
        s.width = Some(-5.0);
        let err = build_scene_with_seed(&[s], 1).unwrap_err();
        assert!(err.contains("width"), "got: {err}");
    }

    #[test]
    fn rejects_single_point_line() {
        let mut s = sk("line", 0.0, 0.0);
        s.points = Some(vec![[0.0, 0.0]]);
        let err = build_scene_with_seed(&[s], 1).unwrap_err();
        assert!(err.contains("at least 2 points"), "got: {err}");
    }

    #[test]
    fn deterministic_for_fixed_seed() {
        let a = build_scene_with_seed(&[sk("rectangle", 0.0, 0.0)], 42).unwrap();
        let b = build_scene_with_seed(&[sk("rectangle", 0.0, 0.0)], 42).unwrap();
        assert_eq!(a, b, "same seed → identical scene (fixture stability)");
    }

    // --- Committed golden fixture ---
    //
    // A representative scene (one of each shape + a labeled container + free
    // text) is generated with a fixed seed and committed at `fixtures/
    // sc6_canvas_golden.excalidraw`. `golden_fixture_matches` fails loudly if
    // the expander's output shape drifts (field added/removed/renamed/default
    // changed). To regenerate after an *intended* change, run the ignored
    // `regen_golden_fixture` and review the diff before committing.

    const GOLDEN_SEED: u64 = 20_260_618;

    fn golden_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/mcp/fixtures/sc6_canvas_golden.excalidraw")
    }

    fn golden_skeletons() -> Vec<CanvasSkeleton> {
        let mut rect = sk("rectangle", 40.0, 40.0);
        rect.width = Some(160.0);
        rect.height = Some(60.0);
        rect.label = Some(CanvasLabel {
            text: "Submit".into(),
            font_size: None,
        });

        let mut ellipse = sk("ellipse", 240.0, 40.0);
        ellipse.background_color = Some("#a5d8ff".into());

        let diamond = sk("diamond", 40.0, 160.0);

        let mut text = sk("text", 240.0, 160.0);
        text.text = Some("hello canvas".into());

        let mut arrow = sk("arrow", 40.0, 280.0);
        arrow.points = Some(vec![[0.0, 0.0], [200.0, 0.0]]);

        let line = sk("line", 40.0, 320.0);

        vec![rect, ellipse, diamond, text, arrow, line]
    }

    #[test]
    fn golden_fixture_matches() {
        let produced = build_scene_with_seed(&golden_skeletons(), GOLDEN_SEED).unwrap();
        let golden = std::fs::read_to_string(golden_path()).expect(
            "golden fixture missing — generate it with \
             `cargo test -p codesense regen_golden_fixture -- --ignored`",
        );
        assert_eq!(
            produced.trim(),
            golden.trim(),
            "expander output drifted from the committed golden fixture. If this \
             change was intentional, regenerate with `cargo test -p codesense \
             regen_golden_fixture -- --ignored` and review the diff."
        );
    }

    #[test]
    #[ignore = "regenerates the committed golden fixture; run manually after an intended expander change"]
    fn regen_golden_fixture() {
        let produced = build_scene_with_seed(&golden_skeletons(), GOLDEN_SEED).unwrap();
        let path = golden_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, produced).unwrap();
    }
}
