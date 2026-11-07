// SPDX-License-Identifier: MIT
//! Local acceptance and evaluation of spend caps. Team files express requests;
//! only native acceptance can loosen the local ratchet.
use super::{db_err, SpendScope, SpendWindow, UsageStore};
use chrono::NaiveDate;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub per_day: Option<f64>,
    pub per_week: Option<f64>,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BudgetUsd {
    pub per_day: Option<f64>,
    pub per_week: Option<f64>,
}
impl From<BudgetUsd> for Budget {
    fn from(b: BudgetUsd) -> Self {
        Self {
            per_day: b.per_day,
            per_week: b.per_week,
        }
    }
}
impl From<Budget> for BudgetUsd {
    fn from(b: Budget) -> Self {
        Self {
            per_day: b.per_day,
            per_week: b.per_week,
        }
    }
}
impl Budget {
    pub fn validate(self) -> Result<(), String> {
        for (field, value) in [("per_day", self.per_day), ("per_week", self.per_week)] {
            if value.is_some_and(|v| !v.is_finite() || v <= 0.0) {
                return Err(format!("budget_usd.{field} must be positive finite USD"));
            }
        }
        Ok(())
    }
    fn lower(self, other: Self) -> Self {
        let min = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Self {
            per_day: min(self.per_day, other.per_day),
            per_week: min(self.per_week, other.per_week),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendSummary {
    pub today: f64,
    pub week: f64,
    pub state: &'static str,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapStatus {
    pub budget_usd: Option<BudgetUsd>,
    pub budget_pending: bool,
    pub spend: SpendSummary,
    #[serde(skip)]
    pub effective: Budget,
    #[serde(skip)]
    pub unpriced_models: Vec<String>,
    #[serde(skip)]
    pub pending_rates: Vec<super::SetModelRateParams>,
    #[serde(skip)]
    pub pending_at: Option<i64>,
    #[serde(skip)]
    pub refusal: Option<String>,
    #[serde(skip)]
    pub alerts: Vec<CapAlert>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct CapAlert {
    pub key: String,
    pub level: u8,
    pub detail: String,
}

impl UsageStore {
    pub(super) fn caps_schema(&self) -> Result<(), String> {
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS spend_cap (kind TEXT NOT NULL, scope_key TEXT NOT NULL, accepted TEXT NOT NULL, generation INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(kind,scope_key));
CREATE TABLE IF NOT EXISTS spend_cap_edit (kind TEXT NOT NULL, scope_key TEXT NOT NULL, request TEXT NOT NULL, seen_at INTEGER NOT NULL, PRIMARY KEY(kind,scope_key));
CREATE TABLE IF NOT EXISTS spend_cap_notice (notice_key TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS pending_model_rate (model_prefix TEXT NOT NULL, effective_from TEXT NOT NULL, request TEXT NOT NULL, PRIMARY KEY(model_prefix,effective_from));").map_err(db_err)
    }
    pub fn accept_cap(&mut self, kind: &str, key: &str, budget: Budget) -> Result<(), String> {
        budget.validate()?;
        self.caps_schema()?;
        self.conn.execute("INSERT INTO spend_cap(kind,scope_key,accepted,generation) VALUES (?1,?2,?3,1) ON CONFLICT(kind,scope_key) DO UPDATE SET accepted=excluded.accepted,generation=spend_cap.generation+1",params![kind,key,serde_json::to_string(&budget).map_err(|e|e.to_string())?]).map_err(db_err)?;
        changed();
        Ok(())
    }
    /// `unrecorded`: this scope's sessions send their usage elsewhere (managed Claude settings),
    /// so a set cap reports `spend.state: "unrecorded"`. It is advisory: nothing is refused for it.
    pub fn cap_status(
        &mut self,
        kind: &str,
        key: &str,
        file: Budget,
        day: NaiveDate,
        unrecorded: bool,
    ) -> Result<CapStatus, String> {
        file.validate()?;
        self.caps_schema()?;
        let (effective, generation) = {
            let tx = self
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(db_err)?;
            let previous: Option<(String, u64)> = tx
                .query_row(
                    "SELECT accepted,generation FROM spend_cap WHERE kind=?1 AND scope_key=?2",
                    params![kind, key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(db_err)?;
            let (raw, generation) = previous.unwrap_or_else(|| ("{}".into(), 0));
            let accepted: Budget = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            let effective = accepted.lower(file);
            let generation = generation + u64::from(effective != accepted);
            // A read that changes nothing writes nothing: the row (absent is the unset cap) is
            // written only when the file lowers what was accepted.
            if effective != accepted {
                tx.execute("INSERT INTO spend_cap(kind,scope_key,accepted,generation) VALUES (?1,?2,?3,?4) ON CONFLICT(kind,scope_key) DO UPDATE SET accepted=excluded.accepted,generation=excluded.generation",params![kind,key,serde_json::to_string(&effective).map_err(|e|e.to_string())?,generation]).map_err(db_err)?;
            }
            tx.commit().map_err(db_err)?;
            (effective, generation)
        };
        let scope = if kind == "agent" {
            SpendScope::Agent(key)
        } else {
            SpendScope::Routine(key)
        };
        let today = self.spend_for(scope, SpendWindow::Day(day))?;
        let week = self.spend_for(scope, SpendWindow::Week(day))?;
        let mut status = CapStatus {
            budget_usd: (file != Budget::default()).then(|| file.into()),
            budget_pending: effective != file,
            spend: SpendSummary {
                today: today.cost_usd,
                week: week.cost_usd,
                state: "ok",
            },
            effective,
            unpriced_models: Vec::new(),
            pending_rates: Vec::new(),
            pending_at: None,
            refusal: None,
            alerts: Vec::new(),
        };
        use chrono::Datelike;
        let monday = day - chrono::Duration::days(day.weekday().num_days_from_monday() as i64);
        for (window, start, cap, spend) in [
            ("day", day, effective.per_day, &today),
            ("week", monday, effective.per_week, &week),
        ] {
            let Some(cap) = cap else { continue };
            let unpriced = !spend.unpriced_models.is_empty();
            let level = if unpriced || spend.cost_usd >= cap {
                100
            } else if spend.cost_usd >= cap * 0.8 {
                80
            } else {
                0
            };
            let detail = if unpriced {
                format!("{kind} {key}, local {window} from {start}: can't price {}; set a rate (priced spend ${:.4}, cap ${cap:.4})", spend.unpriced_models.join(", "), spend.cost_usd)
            } else {
                format!(
                    "{kind} {key}, local {window} from {start}: spend ${:.4}, cap ${cap:.4}",
                    spend.cost_usd
                )
            };
            if level == 100 && status.refusal.is_none() {
                status.refusal = Some(detail.clone());
            }
            if unpriced {
                status
                    .unpriced_models
                    .extend(spend.unpriced_models.iter().cloned());
                status.unpriced_models.sort();
                status.unpriced_models.dedup();
                status.spend.state = "unpriced";
            } else if status.spend.state != "unpriced" && level == 100 {
                status.spend.state = "reached";
            } else if status.spend.state == "ok" && level == 80 {
                status.spend.state = "warn";
            }
            // Both threshold notices are emitted if an export crosses both at once.
            for threshold in [80, 100] {
                if level >= threshold {
                    status.alerts.push(CapAlert {
                        key: format!("{kind}:{key}:{window}:{start}:{threshold}"),
                        level: threshold,
                        detail: detail.clone(),
                    });
                }
            }
        }
        // The recorded numbers stay; the state says they are not the real spend. A refusal keeps
        // its "reached" or "unpriced" state, so its set-rate action still shows.
        if unrecorded && effective != Budget::default() && status.refusal.is_none() {
            status.spend.state = "unrecorded";
        }
        if status.budget_pending {
            let request = format!(
                "{generation}:{}",
                serde_json::to_string(&file).map_err(|e| e.to_string())?
            );
            self.conn.execute("INSERT INTO spend_cap_edit(kind,scope_key,request,seen_at) VALUES (?1,?2,?3,?4) ON CONFLICT(kind,scope_key) DO UPDATE SET request=excluded.request,seen_at=CASE WHEN spend_cap_edit.request=excluded.request THEN spend_cap_edit.seen_at ELSE excluded.seen_at END", params![kind,key,request,chrono::Utc::now().timestamp_millis()]).map_err(db_err)?;
            status.pending_at = Some(
                self.conn
                    .query_row(
                        "SELECT seen_at FROM spend_cap_edit WHERE kind=?1 AND scope_key=?2",
                        params![kind, key],
                        |r| r.get(0),
                    )
                    .map_err(db_err)?,
            );
            status.alerts.push(CapAlert {
                key: format!(
                    "{kind}:{key}:pending:{generation}:{}",
                    serde_json::to_string(&file).map_err(|e| e.to_string())?
                ),
                level: 0,
                detail: format!(
                    "{kind} {key}: spend cap raise/removal awaits a person's native acceptance"
                ),
            });
        }
        Ok(status)
    }
    pub fn first_cap_notice(&self, key: &str) -> Result<bool, String> {
        self.caps_schema()?;
        self.conn
            .execute("INSERT OR IGNORE INTO spend_cap_notice VALUES (?1)", [key])
            .map(|n| n > 0)
            .map_err(db_err)
    }
}

pub fn status(
    home: &Path,
    kind: &str,
    key: &str,
    file: Option<Budget>,
) -> Result<CapStatus, String> {
    status_with(home, kind, key, file, false)
}

/// [`status`], with whether the scope's spend is unrecorded on this Mac (see
/// [`UsageStore::cap_status`]).
pub fn status_with(
    home: &Path,
    kind: &str,
    key: &str,
    file: Option<Budget>,
    unrecorded: bool,
) -> Result<CapStatus, String> {
    UsageStore::open(&home.join("usage.sqlite"))?.cap_status(
        kind,
        key,
        file.unwrap_or_default(),
        chrono::Local::now().date_naive(),
        unrecorded,
    )
}

/// An agent's [`status`]: unrecorded when its runtime's usage goes elsewhere on this Mac.
pub fn agent_status(
    home: &Path,
    key: &str,
    agent: &crate::agents::model::Agent,
) -> Result<CapStatus, String> {
    let unrecorded = super::managed_telemetry::unrecorded(agent.runtime);
    status_with(home, "agent", key, agent.budget_usd, unrecorded)
}

/// Whether a routine's spend is unrecorded on this Mac: its agent's runtime decides, and a
/// routine with no agent (or one that does not resolve) is recorded.
pub(crate) fn routine_unrecorded(
    home: &Path,
    principal: &crate::domain::Principal,
    agent: Option<&crate::agents::AgentKey>,
) -> bool {
    super::managed_telemetry::redirect().is_some()
        && agent
            .and_then(|key| crate::agents::service::read_current(home, principal, key).ok())
            .is_some_and(|agent| super::managed_telemetry::unrecorded(agent.runtime))
}
static SESSION_HOLDS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::BTreeMap<String, Option<String>>>,
> = std::sync::LazyLock::new(Default::default);

pub(crate) fn refuse_held_session(session: &str) -> Result<(), String> {
    if let Some(reason) = SESSION_HOLDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(session)
        .cloned()
        .flatten()
    {
        return Err(reason);
    }
    Ok(())
}

pub(crate) async fn refuse_session_input(
    app: &tauri::AppHandle,
    session: &str,
) -> Result<(), String> {
    refuse_held_session(session)?;
    match session_cap_status(app, session)
        .await?
        .and_then(|s| s.refusal)
    {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

pub(crate) async fn refuse_mail_offer(app: &tauri::AppHandle, session: &str) -> Result<(), String> {
    if let Some(status) = session_cap_status(app, session).await? {
        let local_host = if status.effective != Budget::default() {
            let app = app.clone();
            let session = session.to_string();
            crate::commands::await_blocking("spend_caps.mail_host", move || {
                crate::commands::host_inbox::require_mail_spend_holds(&app, &session)
            })
            .await
        } else {
            Ok(())
        };
        admit_mail_offer(&status, local_host)?;
    }
    refuse_held_session(session)
}

pub(crate) fn admit_mail_offer(
    status: &CapStatus,
    local_host: Result<(), String>,
) -> Result<(), String> {
    if status.effective != Budget::default() {
        local_host?;
    }
    status.refusal.clone().map_or(Ok(()), Err)
}

async fn session_cap_status(
    app: &tauri::AppHandle,
    session: &str,
) -> Result<Option<CapStatus>, String> {
    use tauri::Manager;
    let revision = app
        .try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
        .and_then(|state| state.agent_revision_for_session(session));
    let key = match revision {
        Some(revision) => crate::agents::AgentRevisionRef::key_of(&revision),
        None => crate::mcp::terminal_tabs::LiveTabs::read(app)
            .await
            .tabs
            .into_iter()
            .find(|tab| tab.session == session)
            .and_then(|tab| tab.agent)
            .and_then(|key| crate::agents::AgentKey::parse(&key)),
    };
    let Some(key) = key else {
        return Ok(None);
    };
    let caller = crate::commands::routines::caller(app).await?;
    crate::commands::await_blocking("spend_caps.input", move || {
        let agent = crate::agents::service::get(&caller.home, &caller.principal, &key)
            .map_err(|e| e.to_string())?
            .agent;
        status(&caller.home, "agent", &key.to_string(), agent.budget_usd).map(Some)
    })
    .await
}

pub fn refuse_agent(
    home: &Path,
    principal: &crate::domain::Principal,
    key: &crate::agents::AgentKey,
) -> Result<(), String> {
    let agent = crate::agents::service::get(home, principal, key)
        .map_err(|e| e.to_string())?
        .agent;
    match agent_status(home, &key.to_string(), &agent)?.refusal {
        Some(why) => Err(why),
        None => Ok(()),
    }
}
pub fn refuse_routine(
    home: &Path,
    principal: &crate::domain::Principal,
    key: &crate::routines::RoutineKey,
    def: &crate::routines::RoutineDef,
) -> Result<(), String> {
    let caps = status(home, "routine", &key.to_string(), def.policy.budget_usd)?;
    validate_routine_cap(def, caps.effective).map_err(|e| e.to_string())?;
    if let Some(why) = caps.refusal {
        return Err(why);
    }
    if !def.is_command() {
        if let Ok(agent) = def.agent.key_for(key) {
            if crate::agents::service::get(home, principal, &agent).is_ok() {
                refuse_agent(home, principal, &agent)?;
            }
        }
    }
    Ok(())
}

/// Pending removal retains the same mode/arming restrictions as a cap in the file.
pub(crate) fn validate_routine_cap(
    def: &crate::routines::RoutineDef,
    effective: Budget,
) -> Result<(), crate::routines::model::ValidationError> {
    if effective != Budget::default() {
        let mut governed = def.clone();
        governed.policy.budget_usd = Some(effective);
        governed.validate()?;
    }
    Ok(())
}

pub fn accept_current(
    home: &Path,
    principal: &crate::domain::Principal,
    kind: &str,
    key: &str,
    expected: Option<Budget>,
) -> Result<(), String> {
    let file = read_budget(home, principal, kind, key)?;
    if file.unwrap_or_default() != expected.unwrap_or_default() {
        return Err("spend cap changed; reload and review it before accepting".into());
    }
    UsageStore::open(&home.join("usage.sqlite"))?.accept_cap(kind, key, file.unwrap_or_default())
}

fn read_budget(
    home: &Path,
    principal: &crate::domain::Principal,
    kind: &str,
    key: &str,
) -> Result<Option<Budget>, String> {
    Ok(match kind {
        "agent" => {
            crate::agents::service::get(
                home,
                principal,
                &crate::agents::AgentKey::parse(key).ok_or("invalid agent key")?,
            )
            .map_err(|e| e.to_string())?
            .agent
            .budget_usd
        }
        "routine" => {
            crate::routines::service::get_current(
                home,
                principal,
                &crate::routines::RoutineKey::parse(key).ok_or("invalid routine key")?,
            )
            .map_err(|e| e.to_string())?
            .def
            .policy
            .budget_usd
        }
        _ => return Err("scopeKind must be agent or routine".into()),
    })
}

/// A person's native edit saves the file and accepts precisely those values.
pub fn set_current(
    home: &Path,
    principal: &crate::domain::Principal,
    kind: &str,
    key: &str,
    budget: Option<Budget>,
) -> Result<(), String> {
    set_reviewed(home, principal, kind, key, budget, None)
}

/// Keep resolves exactly the displayed request, restoring the already effective cap.
pub fn keep_current(
    home: &Path,
    principal: &crate::domain::Principal,
    kind: &str,
    key: &str,
    expected: Option<Budget>,
) -> Result<(), String> {
    if read_budget(home, principal, kind, key)?.unwrap_or_default() != expected.unwrap_or_default()
    {
        return Err("spend cap changed; reload and review it before keeping".into());
    }
    let effective = status(home, kind, key, expected)?.effective;
    set_reviewed(
        home,
        principal,
        kind,
        key,
        (effective != Budget::default()).then_some(effective),
        Some(expected),
    )
}

fn set_reviewed(
    home: &Path,
    principal: &crate::domain::Principal,
    kind: &str,
    key: &str,
    budget: Option<Budget>,
    expected: Option<Option<Budget>>,
) -> Result<(), String> {
    budget.unwrap_or_default().validate()?;
    let author = crate::routines::Author::user(principal.as_str());
    match kind {
        "agent" => {
            let key = crate::agents::AgentKey::parse(key).ok_or("invalid agent key")?;
            let mut current =
                crate::agents::service::get(home, principal, &key).map_err(|e| e.to_string())?;
            if expected.is_some_and(|e| {
                e.unwrap_or_default() != current.agent.budget_usd.unwrap_or_default()
            }) {
                return Err("spend cap changed; reload and review it before keeping".into());
            }
            let target = crate::agents::service::SaveTarget::Update {
                key,
                base_revision: current.agent.revision,
            };
            current.agent.budget_usd = budget;
            crate::agents::service::save(home, principal, target, current.agent, &author)
                .map_err(|e| e.to_string())?;
        }
        "routine" => {
            let key = crate::routines::RoutineKey::parse(key).ok_or("invalid routine key")?;
            let mut current = crate::routines::service::get_current(home, principal, &key)
                .map_err(|e| e.to_string())?;
            if expected.is_some_and(|e| {
                e.unwrap_or_default() != current.def.policy.budget_usd.unwrap_or_default()
            }) {
                return Err("spend cap changed; reload and review it before keeping".into());
            }
            let target = crate::routines::service::SaveTarget::Update {
                key,
                base_revision: current.def.revision,
            };
            current.def.policy.budget_usd = budget;
            crate::routines::service::save(home, principal, target, current.def, &author)
                .map_err(|e| e.to_string())?;
        }
        _ => return Err("scopeKind must be agent or routine".into()),
    }
    accept_current(home, principal, kind, key, budget)
}

fn wake() -> &'static tokio::sync::Notify {
    static WAKE: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();
    WAKE.get_or_init(Default::default)
}
pub fn changed() {
    wake().notify_one();
}

impl UsageStore {
    pub fn request_rate(
        &mut self,
        p: super::SetModelRateParams,
        source: &str,
    ) -> Result<usize, String> {
        self.caps_schema()?;
        let rate = p.rate();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_err)?;
        // Compare every effective period this prefix can affect, under the same write lock.
        // A historical specific rate can replace a later, more expensive family rate.
        let prefix = p.model_prefix.replace('.', "-");
        let mut points = vec![(prefix.clone(), p.effective_from.clone())];
        {
            let key = super::model_key_sql("model");
            let mut stmt = tx.prepare(&format!("SELECT model_prefix,effective_from FROM model_rate WHERE (model_prefix=?1 OR model_prefix LIKE ?1 || '-%') AND effective_from>=?2 UNION SELECT {key},day FROM usage_event WHERE ({key}=?1 OR {key} LIKE ?1 || '-%') AND day>=?2")).map_err(db_err)?;
            let rows = stmt
                .query_map(params![prefix, p.effective_from], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(db_err)?;
            for row in rows {
                points.push(row.map_err(db_err)?);
            }
            // Later changes of a parent prefix affect the new specific prefix too.
            let mut stmt = tx.prepare("SELECT effective_from FROM model_rate WHERE (?1=model_prefix OR ?1 LIKE model_prefix || '-%') AND effective_from>=?2").map_err(db_err)?;
            let rows = stmt
                .query_map(params![prefix, p.effective_from], |r| r.get::<_, String>(0))
                .map_err(db_err)?;
            for row in rows {
                points.push((prefix.clone(), row.map_err(db_err)?));
            }
        }
        let before = points
            .iter()
            .map(|(model, day)| super::rate_on(&tx, model, day))
            .collect::<Result<Vec<_>, _>>()?;
        let previous: Option<String> = tx.query_row("SELECT request FROM pending_model_rate WHERE model_prefix=?1 AND effective_from=?2", params![prefix,p.effective_from], |r| r.get(0)).optional().map_err(db_err)?;
        if previous.as_deref() != Some(&serde_json::to_string(&p).map_err(|e| e.to_string())?) {
            clear_pending_rate_notice(&tx, &prefix, &p.effective_from)?;
        }
        tx.execute_batch("SAVEPOINT rate_preview").map_err(db_err)?;
        let (prefix, day) =
            super::insert_rate(&tx, &p.model_prefix, &p.effective_from, &rate, source)?;
        let mut lowers = false;
        for ((model, date), old) in points.iter().zip(before) {
            let new = super::rate_on(&tx, model, date)?;
            lowers |= match (old, new) {
                (None, Some(_)) => true,
                (Some(old), Some(new)) => rate_lowers(old, new),
                _ => false,
            };
        }
        let count = if lowers {
            tx.execute_batch("ROLLBACK TO rate_preview; RELEASE rate_preview")
                .map_err(db_err)?;
            tx.execute("INSERT INTO pending_model_rate VALUES (?1,?2,?3) ON CONFLICT(model_prefix,effective_from) DO UPDATE SET request=excluded.request",params![prefix,day,serde_json::to_string(&p).map_err(|e|e.to_string())?]).map_err(db_err)?;
            0
        } else {
            tx.execute_batch("RELEASE rate_preview").map_err(db_err)?;
            let count = super::reprice_covered(&tx, &prefix, &day)?;
            clear_pending_rate_notice(&tx, &prefix, &day)?;
            tx.execute(
                "DELETE FROM pending_model_rate WHERE model_prefix=?1 AND effective_from=?2",
                params![prefix, day],
            )
            .map_err(db_err)?;
            count
        };
        tx.commit().map_err(db_err)?;
        changed();
        Ok(count)
    }
    pub fn accept_rate(
        &mut self,
        prefix: &str,
        day: &str,
        expected: &super::SetModelRateParams,
    ) -> Result<usize, String> {
        self.caps_schema()?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let raw: String = tx.query_row("SELECT request FROM pending_model_rate WHERE model_prefix=?1 AND effective_from=?2",params![prefix.replace('.',"-"),day],|r|r.get(0)).map_err(db_err)?;
        let p: super::SetModelRateParams = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        if &p != expected {
            return Err("pending model rate changed; reload and review it before accepting".into());
        }
        let (prefix, day) = super::insert_rate(
            &tx,
            &p.model_prefix,
            &p.effective_from,
            &p.rate(),
            super::SET_BY_USER,
        )?;
        let count = super::reprice_covered(&tx, &prefix, &day)?;
        clear_pending_rate_notice(&tx, &prefix, &day)?;
        tx.execute(
            "DELETE FROM pending_model_rate WHERE model_prefix=?1 AND effective_from=?2",
            params![prefix, day],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        changed();
        Ok(count)
    }
    pub fn pending_rate(&self, prefix: &str, day: &str) -> Result<bool, String> {
        self.caps_schema()?;
        self.conn.query_row("SELECT EXISTS(SELECT 1 FROM pending_model_rate WHERE model_prefix=?1 AND effective_from=?2)",params![prefix.replace('.',"-"),day],|r|r.get(0)).map_err(db_err)
    }
}
fn clear_pending_rate_notice(
    conn: &rusqlite::Connection,
    prefix: &str,
    day: &str,
) -> Result<(), String> {
    conn.execute("DELETE FROM spend_cap_notice WHERE notice_key IN (SELECT 'pending-rate:' || request FROM pending_model_rate WHERE model_prefix=?1 AND effective_from=?2)",params![prefix,day]).map_err(db_err)?;
    Ok(())
}
fn rate_lowers(old: super::Rate, new: super::Rate) -> bool {
    let coefficients = |r: super::Rate, prompt| {
        let (i, o, c) = match r.long {
            Some(l) if prompt > l.from_tokens => (l.input, l.output, l.cache_read),
            _ => (r.input, r.output, r.cache_read),
        };
        [i, o, c.unwrap_or(i * 0.1)]
    };
    let points = [
        0,
        old.long.map_or(0, |l| l.from_tokens.saturating_add(1)),
        new.long.map_or(0, |l| l.from_tokens.saturating_add(1)),
    ];
    points.into_iter().any(|p| {
        coefficients(old, p)
            .into_iter()
            .zip(coefficients(new, p))
            .any(|(a, b)| b < a)
    })
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Evaluation {
    pub agents: std::collections::BTreeMap<String, CapStatus>,
    pub routines: std::collections::BTreeMap<String, CapStatus>,
    pub routine_agents: std::collections::BTreeMap<String, String>,
    pub pending_rates: Vec<super::SetModelRateParams>,
}
pub fn evaluate(home: &Path, principal: &crate::domain::Principal) -> Result<Evaluation, String> {
    evaluate_sessions(home, principal, &BTreeMap::new())
}

fn evaluate_sessions(
    home: &Path,
    principal: &crate::domain::Principal,
    sessions: &BTreeMap<String, Option<String>>,
) -> Result<Evaluation, String> {
    let mut result = Evaluation::default();
    let mut usage = UsageStore::open(&home.join("usage.sqlite"))?;
    let day = chrono::Local::now().date_naive();
    // Read once for the whole evaluation.
    let redirected = super::managed_telemetry::redirect().is_some();
    let unrecorded = move |runtime: codesense_pty_core::agent_cli::AgentCli| {
        redirected && super::managed_telemetry::redirectable(runtime)
    };
    // Each agent's runtime, so a routine's spend is unrecorded when its agent's is.
    let mut runtimes = BTreeMap::new();
    for a in crate::agents::service::list(home, principal, true).map_err(|e| e.to_string())? {
        let key = a.key.to_string();
        runtimes.insert(key.clone(), a.agent.runtime);
        result.agents.insert(
            key.clone(),
            usage.cap_status(
                "agent",
                &key,
                a.agent.budget_usd.unwrap_or_default(),
                day,
                unrecorded(a.agent.runtime),
            )?,
        );
    }
    // A live private agent can be outside the viewed org. Its revision identifies
    // the scoped key; enforcement still reads today's saved financial policy.
    let live_keys: std::collections::BTreeSet<_> = sessions
        .values()
        .filter_map(|revision| revision.as_deref())
        .filter_map(crate::agents::AgentRevisionRef::key_of)
        .collect();
    for key in live_keys {
        let rendered = key.to_string();
        if result.agents.contains_key(&rendered) {
            continue;
        }
        // Leave unresolved identities absent so hold propagation refuses them.
        if let Ok(agent) = crate::agents::service::read_current(home, principal, &key) {
            runtimes.insert(rendered.clone(), agent.runtime);
            result.agents.insert(
                rendered.clone(),
                usage.cap_status(
                    "agent",
                    &rendered,
                    agent.budget_usd.unwrap_or_default(),
                    day,
                    unrecorded(agent.runtime),
                )?,
            );
        }
    }
    for r in crate::routines::service::list_summaries(home, principal).map_err(|e| e.to_string())? {
        let key = r.key.to_string();
        let agent = r.def.agent.key_for(&r.key).ok().map(|agent| agent.to_string());
        if let Some(agent) = &agent {
            result.routine_agents.insert(key.clone(), agent.clone());
        }
        let runtime = agent
            .filter(|_| !r.def.is_command())
            .and_then(|agent| runtimes.get(&agent).copied());
        result.routines.insert(
            key.clone(),
            usage.cap_status(
                "routine",
                &key,
                r.def.policy.budget_usd.unwrap_or_default(),
                day,
                runtime.is_some_and(unrecorded),
            )?,
        );
    }
    usage.caps_schema()?;
    let mut pending = usage
        .conn
        .prepare("SELECT request FROM pending_model_rate ORDER BY model_prefix,effective_from")
        .map_err(db_err)?;
    let rows = pending
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(db_err)?;
    for row in rows {
        result
            .pending_rates
            .push(serde_json::from_str(&row.map_err(db_err)?).map_err(|e| e.to_string())?);
    }
    use chrono::Datelike;
    let monday = day - chrono::Duration::days(day.weekday().num_days_from_monday() as i64);
    for (kind, scopes) in [
        ("agent", &mut result.agents),
        ("routine", &mut result.routines),
    ] {
        for (key, status) in scopes {
            if status.effective == Budget::default() {
                continue;
            }
            let start = if status.effective.per_week.is_some() {
                monday
            } else {
                day
            };
            for request in &result.pending_rates {
                let prefix = request.model_prefix.replace('.', "-");
                let model = super::model_key_sql("e.model");
                let covered: bool = usage.conn.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM usage_event e JOIN session_attribution a ON a.session_id=e.session_id WHERE ((?5='agent' AND a.agent=?1) OR (?5='routine' AND a.handle >= 'routine:' || ?1 || '@' AND a.handle < 'routine:' || ?1 || 'A')) AND e.day>=?2 AND e.day>=?3 AND ({model}=?4 OR {model} LIKE ?4 || '-%'))"),
                params![key, start.to_string(), request.effective_from, prefix, kind], |r| r.get(0)
            ).map_err(db_err)?;
                if covered {
                    status.pending_rates.push(request.clone());
                }
            }
        }
    }
    Ok(result)
}

pub fn install(app: tauri::AppHandle) {
    use tauri::Listener;
    for name in [
        "routines:changed",
        "agents-changed",
        "agents:changed",
        "context-index-changed",
        "content-changed",
        "terminal-layout-changed",
        "terminal-layout-resync",
        "sessions-source-changed",
        "local-profiles-changed",
    ] {
        app.listen(name, |_| changed());
    }
    if let Some(host) = crate::commands::pty_backend::in_process_host_handle(&app) {
        host.activity().inbox().require_wake_holds();
    }
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(error) = refresh(&app).await {
                log::warn!(target:"spend_caps","spend evaluation: {error}");
            }
            let now = chrono::Local::now();
            let next = now
                .date_naive()
                .succ_opt()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .and_then(|d| d.and_local_timezone(chrono::Local).earliest());
            let wait = next
                .map(|t| (t - now).to_std().unwrap_or_default())
                .unwrap_or(std::time::Duration::from_secs(60));
            tokio::select! { _ = wake().notified() => {}, _ = tokio::time::sleep(wait) => {} }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}

fn session_revisions(
    layouts: Vec<crate::commands::machines::RuntimeLayoutReply>,
    launches: Vec<crate::commands::dashboard_launch::SessionSnapshot>,
) -> BTreeMap<String, Option<String>> {
    let mut decisions = std::collections::BTreeMap::new();
    for layout in layouts {
        if let crate::commands::machines::RuntimeLayoutReply::Live { snapshot, .. } = layout {
            for workspace in snapshot.workspaces.values() {
                for tab in &workspace.tabs {
                    let key = tab
                        .tags
                        .get(crate::commands::dashboard_launch::TAG_LAUNCH_AGENT_REVISION)
                        .cloned();
                    decisions.insert(tab.session.as_str().to_string(), key);
                }
            }
        }
    }
    for row in launches
        .into_iter()
        .filter(|r| r.kind == crate::commands::dashboard_launch::KIND_HEADLESS)
    {
        if let Some(session) = row.session_id {
            let key = row.agent_revision;
            decisions.insert(session, key);
        }
    }
    decisions
}

pub(crate) async fn evaluate_and_publish<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    sessions: &BTreeMap<String, Option<String>>,
    #[cfg(test)] hook: Option<Box<dyn Fn() + Send>>,
) -> Result<
    (
        crate::commands::routines::Caller,
        Result<Evaluation, String>,
    ),
    String,
> {
    let caller = crate::commands::routines::caller(app).await?;
    let home = caller.home.clone();
    let principal = caller.principal.clone();
    let evaluation_sessions = sessions.clone();
    let result = crate::commands::await_blocking("spend_caps.evaluate", move || {
        #[cfg(test)]
        super::SPEND_HOOK.with(|h| {
            *h.borrow_mut() = hook.map(|f| f as Box<dyn Fn()>);
        });
        let result = evaluate_sessions(&home, &principal, &evaluation_sessions);
        #[cfg(test)]
        super::SPEND_HOOK.with(|h| *h.borrow_mut() = None);
        result
    })
    .await;
    if let Ok(value) = &result {
        crate::live_hub::spend_changed(app, value)?;
    }
    Ok((caller, result))
}

async fn refresh(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let layouts = crate::commands::pty_layout::webview_local_layout_snapshots(app.clone()).await;
    let launches = app
        .try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
        .map(|launch| launch.snapshot())
        .unwrap_or_default();
    let decisions = session_revisions(layouts.and_then(Result::ok).unwrap_or_default(), launches);
    #[cfg(test)]
    let (caller, result) = evaluate_and_publish(app, &decisions, None).await?;
    #[cfg(not(test))]
    let (caller, result) = evaluate_and_publish(app, &decisions).await?;
    let hosts = crate::commands::host_inbox::local_hosts(app);
    let app_holds = app.clone();
    let value = crate::commands::await_blocking("spend_caps.holds", move || {
        if let Err(error) = push_session_holds(&result, decisions.clone(), &hosts) {
            log::warn!(target:"spend_caps", "spend hold propagation: {error}");
        }
        let value = result?;
        let mut unavailable = std::collections::BTreeMap::new();
        for (session, revision) in decisions {
            if let Some(key) = revision
                .and_then(|rev| crate::agents::AgentRevisionRef::key_of(&rev))
                .map(|key| key.to_string())
            {
                if value
                    .agents
                    .get(&key)
                    .is_some_and(|s| s.effective != Budget::default())
                {
                    if let Err(reason) =
                        crate::commands::host_inbox::require_mail_spend_holds(&app_holds, &session)
                    {
                        unavailable.insert(key, reason);
                    }
                }
            }
        }
        Ok(with_hold_result(value, unavailable))
    })
    .await?;
    let notify = crate::routines::wake_app::AppWakeEnv::new(app.clone());
    crate::commands::await_blocking("spend_caps.notices", move || {
        let usage = UsageStore::open(&caller.home.join("usage.sqlite"))?;
        notify_evaluation(&usage, &value, &notify)
    })
    .await
}

fn with_hold_result(
    mut value: Evaluation,
    unavailable: std::collections::BTreeMap<String, String>,
) -> Evaluation {
    for (key, error) in unavailable {
        if let Some(status) = value.agents.get_mut(&key) {
            status.alerts.push(CapAlert {
                key: format!("hold-update:{key}:{}", chrono::Local::now().date_naive()), level: 0,
                detail: format!("Agent {key}: spend cap enforcement needs a terminal-server update. No new automatic mail is offered to capped sessions on older local hosts; mail queued before the cap existed may still start a turn. {error}"),
            });
        }
    }
    value
}

/// Production propagation boundary: current evaluation and live session ownership go to hosts.
pub(crate) fn push_session_holds(
    result: &Result<Evaluation, String>,
    sessions: impl IntoIterator<Item = (String, Option<String>)>,
    hosts: &[std::sync::Arc<dyn codesense_pty_core::domain::TerminalDomain>],
) -> Result<(), String> {
    let holds: Vec<_> = sessions
        .into_iter()
        .map(|(session, revision)| {
            let reason = revision.as_deref().and_then(|revision| {
                let key = revision.split('@').next().unwrap_or(revision);
                match result {
                    Ok(value) => value
                        .agents
                        .get(key)
                        .map(|s| s.refusal.clone())
                        .unwrap_or_else(|| {
                            Some(format!("agent {key}: spend cap could not be evaluated"))
                        }),
                    Err(error) => Some(format!("agent {key}: spend unavailable: {error}")),
                }
            });
            codesense_pty_core::inbox_host::WakeHold { session, reason }
        })
        .collect();
    let generation = chrono::Utc::now()
        .timestamp_nanos_opt()
        .ok_or("clock cannot version wake holds")? as u64;
    SESSION_HOLDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .extend(holds.iter().map(|h| (h.session.clone(), h.reason.clone())));
    let mut failure = None;
    for host in hosts {
        if let Err(error) = host.inbox_wake_holds(generation, holds.clone()) {
            failure = Some(error.to_string());
        }
    }
    failure.map_or(Ok(()), Err)
}

pub(crate) fn notify_evaluation(
    usage: &UsageStore,
    value: &Evaluation,
    notify: &dyn crate::routines::wake::WakeNotify,
) -> Result<(), String> {
    use crate::routines::wake::{NoticeKind, WakeNotice};
    usage.caps_schema()?;
    let post = |key: &str, alert: &CapAlert| -> Result<(), String> {
        let seen: bool = usage
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM spend_cap_notice WHERE notice_key=?1)",
                [&alert.key],
                |r| r.get(0),
            )
            .map_err(db_err)?;
        if !seen
            && notify.post_confirmed(&WakeNotice {
                kind: match alert.level {
                    80 => NoticeKind::SpendWarn,
                    100 => NoticeKind::SpendReached,
                    _ => NoticeKind::SpendPending,
                },
                run_id: String::new(),
                routine_key: key.into(),
                machine: "local".into(),
                session: String::new(),
                detail: alert.detail.clone(),
            })
        {
            usage.first_cap_notice(&alert.key)?;
        }
        Ok(())
    };
    for (key, status) in &value.agents {
        for alert in &status.alerts {
            post(key, alert)?;
        }
    }
    for (key, status) in &value.routines {
        for alert in &status.alerts {
            post(
                value
                    .routine_agents
                    .get(key)
                    .map(String::as_str)
                    .unwrap_or(key),
                alert,
            )?;
        }
    }
    for request in &value.pending_rates {
        let json = serde_json::to_string(request).map_err(|e| e.to_string())?;
        let target = value
            .agents
            .iter()
            .find(|(_, s)| s.pending_rates.contains(request))
            .map(|(key, _)| key.clone())
            .or_else(|| {
                value
                    .routines
                    .iter()
                    .find(|(_, s)| s.pending_rates.contains(request))
                    .map(|(key, _)| {
                        format!(
                            "{}#routine:{key}",
                            value.routine_agents.get(key).unwrap_or(key)
                        )
                    })
            })
            .unwrap_or_default();
        post(&target, &CapAlert {
            key: format!("pending-rate:{json}"),
            level: 0,
            detail: format!("Model rate {} from {} awaits native acceptance: input ${}/M, output ${}/M, cache read ${}/M{}. Review in Agents → Overview → Spend cap.",
                request.model_prefix, request.effective_from, request.input_per_mtok, request.output_per_mtok,
                request.cache_read_per_mtok.unwrap_or(request.input_per_mtok * 0.1),
                request.long.map(|long| format!("; long prompts from {} tokens: input ${}/M, output ${}/M, cache read ${}/M", long.from_tokens, long.input_per_mtok, long.output_per_mtok, long.cache_read_per_mtok.unwrap_or(long.input_per_mtok * 0.1))).unwrap_or_default()),
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_usage::tests::store;

    #[test]
    fn file_raises_and_removals_stay_pending_and_lowering_ratchets() {
        let (_dir, mut usage) = store();
        let cap = |v| Budget {
            per_day: v,
            per_week: None,
        };
        let read = |usage: &mut UsageStore, file| {
            usage
                .cap_status("agent", "a", file, chrono::Local::now().date_naive(), false)
                .unwrap()
        };
        assert!(!read(&mut usage, cap(Some(10.0))).budget_pending);
        assert!(read(&mut usage, cap(Some(20.0))).budget_pending);
        assert_eq!(
            read(&mut usage, cap(Some(3.0))).effective.per_day,
            Some(3.0)
        );
        assert_eq!(
            read(&mut usage, cap(Some(10.0))).effective.per_day,
            Some(3.0)
        );
        assert!(read(&mut usage, cap(None)).budget_pending);
        usage.accept_cap("agent", "a", cap(Some(20.0))).unwrap();
        assert!(!read(&mut usage, cap(Some(20.0))).budget_pending);
    }

    /// Under managed Claude settings that send usage elsewhere, a capped Claude agent's spend is
    /// unrecorded and still not refused; a capped Codex agent and an uncapped Claude one are as
    /// before.
    #[test]
    fn a_capped_claude_agent_is_unrecorded_under_managed_settings() {
        let dir = tempfile::tempdir().unwrap();
        let agent = |runtime: &str, cap: &str| -> crate::agents::model::Agent {
            serde_yaml::from_str(&format!("id: a\nname: A\nruntime: {runtime}\n{cap}")).unwrap()
        };
        let capped = "budget_usd: {per_day: 5}\n";
        let state = |key: &str, agent: &crate::agents::model::Agent| {
            let status = agent_status(dir.path(), key, agent).unwrap();
            assert!(status.refusal.is_none(), "{key}: {:?}", status.refusal);
            status.spend.state
        };
        assert_eq!(state("claude", &agent("claude", capped)), "ok");
        let read_from_its_log = crate::session_usage::managed_telemetry::Fixture::redirecting();
        assert_eq!(state("claude", &agent("claude", capped)), "ok", "Claude's session log is read");
        drop(read_from_its_log);
        let _managed = crate::session_usage::managed_telemetry::Fixture::redirecting_unread();
        assert_eq!(state("claude", &agent("claude", capped)), "unrecorded");
        assert_eq!(state("codex", &agent("codex", capped)), "ok");
        assert_eq!(state("uncapped", &agent("claude", "")), "ok");
    }

    /// Reading a cap from a store that already exists changes none of its files: opening it
    /// replaces no trigger, and a cap the store already holds is not written again.
    #[test]
    fn reading_a_cap_from_an_existing_store_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let files = || -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
            std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| {
                    let path = e.unwrap().path();
                    let bytes = std::fs::read(&path).unwrap();
                    (path, bytes)
                })
                .collect()
        };
        let capped = Some(Budget {
            per_day: Some(5.0),
            per_week: None,
        });
        status(dir.path(), "agent", "capped", capped).unwrap();
        status(dir.path(), "agent", "uncapped", None).unwrap();
        let before = files();
        drop(UsageStore::open(&dir.path().join("usage.sqlite")).unwrap());
        for _ in 0..2 {
            status(dir.path(), "agent", "capped", capped).unwrap();
            status(dir.path(), "agent", "uncapped", None).unwrap();
        }
        assert_eq!(before, files());
        // A lower cap is still accepted at once.
        let lower = Some(Budget {
            per_day: Some(2.0),
            per_week: None,
        });
        assert_eq!(
            status(dir.path(), "agent", "capped", lower)
                .unwrap()
                .effective,
            lower.unwrap()
        );
        assert_ne!(before, files());
    }

    #[test]
    fn only_positive_finite_budgets_are_valid() {
        for value in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(Budget {
                per_day: Some(value),
                per_week: None
            }
            .validate()
            .is_err());
        }
    }
}

#[cfg(test)]
mod enforcement_seams {
    use super::*;
    use crate::session_usage::{
        event::Tokens,
        tests::{event, store},
        SetModelRateParams, SET_BY_AGENT,
    };

    /// Unrecorded spend never masks a refusal: an unpriced or reached cap keeps its state (and
    /// with it the set-rate action); only an unrefused cap reads "unrecorded".
    #[test]
    fn a_refused_cap_keeps_its_state_when_spend_is_unrecorded() {
        let (_dir, mut usage) = store();
        usage
            .attribute("s", "org", None, "a", "routine:r@one", None)
            .unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        usage
            .ingest(&[event(
                "e",
                "s",
                "future-model",
                now,
                Tokens {
                    input: 1_000_000,
                    ..Default::default()
                },
            )])
            .unwrap();
        let day = chrono::Local::now().date_naive();
        let read = |usage: &mut UsageStore, per_day: f64| {
            let cap = Budget {
                per_day: Some(per_day),
                per_week: None,
            };
            let status = usage.cap_status("agent", "a", cap, day, true).unwrap();
            (status.spend.state, status.refusal.is_some())
        };
        assert_eq!(read(&mut usage, 100.0), ("unpriced", true));
        let rate = SetModelRateParams {
            model_prefix: "future-model".into(),
            effective_from: "1970-01-01".into(),
            input_per_mtok: 10.0,
            output_per_mtok: 20.0,
            cache_read_per_mtok: None,
            long: None,
        };
        usage.request_rate(rate.clone(), SET_BY_AGENT).unwrap();
        usage
            .accept_rate("future-model", "1970-01-01", &rate)
            .unwrap();
        assert_eq!(read(&mut usage, 100.0), ("unrecorded", false));
        assert_eq!(read(&mut usage, 5.0), ("reached", true));
    }

    #[test]
    fn unpriced_usage_refuses_and_rate_reductions_wait_for_native_accept() {
        let (_dir, mut usage) = store();
        usage
            .attribute("s", "org", None, "a", "routine:r@one", None)
            .unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        usage
            .ingest(&[event(
                "e",
                "s",
                "future-model",
                now,
                Tokens {
                    input: 1_000_000,
                    ..Default::default()
                },
            )])
            .unwrap();
        let budget = Budget {
            per_day: Some(5.0),
            per_week: None,
        };
        let status = usage
            .cap_status("agent", "a", budget, chrono::Local::now().date_naive(), false)
            .unwrap();
        assert!(status.refusal.unwrap().contains("future-model"));
        let rate = SetModelRateParams {
            model_prefix: "future-model".into(),
            effective_from: "1970-01-01".into(),
            input_per_mtok: 10.0,
            output_per_mtok: 20.0,
            cache_read_per_mtok: None,
            long: None,
        };
        assert_eq!(usage.request_rate(rate.clone(), SET_BY_AGENT).unwrap(), 0);
        assert!(usage.pending_rate("future-model", "1970-01-01").unwrap());
        assert_eq!(
            usage
                .spend_for(
                    SpendScope::Agent("a"),
                    SpendWindow::Day(chrono::Local::now().date_naive())
                )
                .unwrap()
                .unpriced_models,
            vec!["future-model"]
        );
        usage
            .accept_rate("future-model", "1970-01-01", &rate)
            .unwrap();
        let mut cheaper = rate.clone();
        cheaper.input_per_mtok = 1.0;
        usage.request_rate(cheaper.clone(), SET_BY_AGENT).unwrap();
        assert_eq!(
            usage
                .spend_for(
                    SpendScope::Agent("a"),
                    SpendWindow::Day(chrono::Local::now().date_naive())
                )
                .unwrap()
                .cost_usd,
            10.0
        );
        usage
            .accept_rate("future-model", "1970-01-01", &cheaper)
            .unwrap();
        assert_eq!(
            usage
                .spend_for(
                    SpendScope::Agent("a"),
                    SpendWindow::Day(chrono::Local::now().date_naive())
                )
                .unwrap()
                .cost_usd,
            1.0
        );
    }

    #[test]
    fn historical_specific_rates_cannot_lower_later_spend_and_stale_accepts_fail() {
        let (_dir, mut usage) = store();
        usage
            .set_rate_and_reprice(
                "sample-family",
                "1970-01-01",
                super::super::Rate::flat(2.0, 2.0),
                super::super::SET_BY_USER,
            )
            .unwrap();
        usage
            .set_rate_and_reprice(
                "sample-family",
                "2026-01-01",
                super::super::Rate::flat(10.0, 10.0),
                super::super::SET_BY_USER,
            )
            .unwrap();
        let request = SetModelRateParams {
            model_prefix: "sample-family-model".into(),
            effective_from: "2025-01-01".into(),
            input_per_mtok: 5.0,
            output_per_mtok: 5.0,
            cache_read_per_mtok: None,
            long: None,
        };
        usage.request_rate(request.clone(), SET_BY_AGENT).unwrap();
        assert!(usage
            .pending_rate(&request.model_prefix, &request.effective_from)
            .unwrap());
        assert_eq!(
            super::super::rate_on(&usage.conn, &request.model_prefix, "2026-02-01")
                .unwrap()
                .unwrap()
                .input,
            10.0
        );
        let mut changed = request.clone();
        changed.input_per_mtok = 4.0;
        usage.request_rate(changed.clone(), SET_BY_AGENT).unwrap();
        assert!(usage
            .accept_rate(&request.model_prefix, &request.effective_from, &request)
            .unwrap_err()
            .contains("changed"));
        assert!(usage
            .pending_rate(&request.model_prefix, &request.effective_from)
            .unwrap());
        usage
            .accept_rate(&changed.model_prefix, &changed.effective_from, &changed)
            .unwrap();
        assert_eq!(
            super::super::rate_on(&usage.conn, &request.model_prefix, "2026-02-01")
                .unwrap()
                .unwrap()
                .input,
            4.0
        );
    }

    #[test]
    fn cap_threshold_notice_is_once_per_scope_window_level() {
        let (_dir, mut usage) = store();
        usage.attribute("s", "org", None, "a", "h", None).unwrap();
        let mut call = event(
            "e",
            "s",
            "claude-opus-5-5",
            chrono::Utc::now().timestamp_millis(),
            Tokens::default(),
        );
        call.reported_cost_usd = Some(10.0);
        usage.ingest(&[call]).unwrap();
        let day = chrono::Local::now().date_naive();
        let cap = Budget {
            per_day: Some(10.0),
            per_week: None,
        };
        for expected in [true, false] {
            let result = usage.cap_status("agent", "a", cap, day, false).unwrap();
            assert!(result.refusal.as_ref().unwrap().contains("local day"));
            let note = result.alerts.iter().find(|n| n.level == 100).unwrap();
            assert_eq!(usage.first_cap_notice(&note.key).unwrap(), expected);
        }
        assert!(usage
            .cap_status("agent", "a", cap, day.succ_opt().unwrap(), false)
            .unwrap()
            .refusal
            .is_none());
    }
}

#[cfg(test)]
mod notice_seams {
    use super::*;
    use crate::routines::wake::{NoticeKind, WakeNotice, WakeNotify};
    #[derive(Default)]
    struct Notes(std::sync::Mutex<Vec<WakeNotice>>);
    impl WakeNotify for Notes {
        fn post(&self, note: &WakeNotice) {
            self.0.lock().unwrap().push(note.clone());
        }
        fn redispatch(&self, _: crate::routines::admit::RoutinePointer, _: std::time::Duration) {}
    }
    #[test]
    fn unavailable_inbox_retries_and_a_new_rate_approval_posts_again() {
        struct Unavailable;
        impl WakeNotify for Unavailable {
            fn post(&self, _: &WakeNotice) {}
            fn post_confirmed(&self, _: &WakeNotice) -> bool {
                false
            }
            fn redispatch(
                &self,
                _: crate::routines::admit::RoutinePointer,
                _: std::time::Duration,
            ) {
            }
        }
        let (_dir, mut usage) = crate::session_usage::tests::store();
        let request = crate::session_usage::SetModelRateParams {
            model_prefix: "sample-model".into(),
            effective_from: "1970-01-01".into(),
            input_per_mtok: 2.0,
            output_per_mtok: 2.0,
            cache_read_per_mtok: None,
            long: None,
        };
        usage
            .request_rate(request.clone(), crate::session_usage::SET_BY_AGENT)
            .unwrap();
        let value = Evaluation {
            pending_rates: vec![request.clone()],
            ..Default::default()
        };
        notify_evaluation(&usage, &value, &Unavailable).unwrap();
        let notes = Notes::default();
        notify_evaluation(&usage, &value, &notes).unwrap();
        notify_evaluation(&usage, &value, &notes).unwrap();
        assert_eq!(notes.0.lock().unwrap().len(), 1);
        usage
            .accept_rate(&request.model_prefix, &request.effective_from, &request)
            .unwrap();
        usage
            .set_rate_and_reprice(
                &request.model_prefix,
                &request.effective_from,
                crate::session_usage::Rate::flat(10.0, 10.0),
                crate::session_usage::SET_BY_USER,
            )
            .unwrap();
        usage
            .request_rate(request.clone(), crate::session_usage::SET_BY_AGENT)
            .unwrap();
        notify_evaluation(&usage, &value, &notes).unwrap();
        assert_eq!(notes.0.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_cap_change_below_limit_still_posts_the_older_host_update_bound() {
        let (_dir, mut usage) = crate::session_usage::tests::store();
        let status = usage
            .cap_status(
                "agent",
                "worker",
                Budget {
                    per_day: Some(1.0),
                    per_week: None,
                },
                chrono::Local::now().date_naive(),
                false,
            )
            .unwrap();
        assert!(status.refusal.is_none());
        let value = with_hold_result(
            Evaluation {
                agents: [("worker".into(), status)].into(),
                ..Default::default()
            },
            [("worker".into(), "terminal server update".into())].into(),
        );
        let notes = Notes::default();
        notify_evaluation(&usage, &value, &notes).unwrap();
        let notes = notes.0.lock().unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0]
            .detail
            .contains("enforcement needs a terminal-server update"));
        assert!(notes[0]
            .detail
            .contains("mail queued before the cap existed may still start"));
        assert!(!notes[0].detail.contains("automatic mail is held"));
    }

    #[test]
    fn propagation_failure_still_posts_threshold_and_server_update_notices() {
        let (_dir, mut usage) = crate::session_usage::tests::store();
        usage
            .attribute("compat", "org", None, "worker", "h", None)
            .unwrap();
        let mut row = crate::session_usage::tests::event(
            "compat-usage",
            "compat",
            "claude-opus-5-5",
            chrono::Utc::now().timestamp_millis(),
            Default::default(),
        );
        row.reported_cost_usd = Some(2.0);
        usage.ingest(&[row]).unwrap();
        let status = usage
            .cap_status(
                "agent",
                "worker",
                Budget {
                    per_day: Some(1.0),
                    per_week: None,
                },
                chrono::Local::now().date_naive(),
                false,
            )
            .unwrap();
        let value = with_hold_result(
            Evaluation {
                agents: [("worker".into(), status)].into(),
                ..Default::default()
            },
            [(
                "worker".into(),
                "spend holds require a terminal server update".into(),
            )]
            .into(),
        );
        let notes = Notes::default();
        for _ in 0..2 {
            notify_evaluation(&usage, &value, &notes).unwrap();
        }
        let notes = notes.0.lock().unwrap();
        assert_eq!(
            notes
                .iter()
                .filter(|n| n.kind == NoticeKind::SpendReached)
                .count(),
            1
        );
        assert_eq!(
            notes
                .iter()
                .filter(|n| n
                    .detail
                    .contains("enforcement needs a terminal-server update"))
                .count(),
            1
        );
    }

    #[test]
    fn repeated_evaluation_posts_one_reached_notice_and_one_pending_raise() {
        let (_dir, mut usage) = crate::session_usage::tests::store();
        usage.attribute("s", "org", None, "a", "h", None).unwrap();
        let mut event = crate::session_usage::tests::event(
            "e",
            "s",
            "claude-opus-5-5",
            chrono::Utc::now().timestamp_millis(),
            Default::default(),
        );
        event.reported_cost_usd = Some(1.0);
        usage.ingest(&[event]).unwrap();
        let day = chrono::Local::now().date_naive();
        let notes = Notes::default();
        usage
            .cap_status(
                "agent",
                "a",
                Budget {
                    per_day: Some(1.0),
                    per_week: None,
                },
                day,
                false,
            )
            .unwrap();
        for _ in 0..2 {
            let status = usage
                .cap_status(
                    "agent",
                    "a",
                    Budget {
                        per_day: Some(10.0),
                        per_week: None,
                    },
                    day,
                    false,
                )
                .unwrap();
            let value = Evaluation {
                agents: [("a".into(), status)].into(),
                ..Default::default()
            };
            notify_evaluation(&usage, &value, &notes).unwrap();
        }
        let notes = notes.0.lock().unwrap();
        assert_eq!(
            notes
                .iter()
                .filter(|n| n.kind == NoticeKind::SpendReached)
                .count(),
            1
        );
        assert_eq!(
            notes
                .iter()
                .filter(|n| n.kind == NoticeKind::SpendPending)
                .count(),
            1
        );
    }
}

#[cfg(test)]
mod native_edit_seams {
    use super::*;
    use crate::routines::test_support::{agent, alice, save_team_agent, write_org};
    use codesense_pty_core::agent_cli::AgentCli;
    #[test]
    fn native_save_writes_and_accepts_while_a_stale_exact_accept_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let user = alice();
        write_org(home.path(), 1);
        let mut definition = agent("cap-control", AgentCli::Claude);
        definition.budget_usd = Some(Budget {
            per_day: Some(50.0),
            per_week: None,
        });
        let key = save_team_agent(home.path(), &user, definition).to_string();
        let raised = Some(Budget {
            per_day: Some(75.0),
            per_week: None,
        });
        set_current(home.path(), &user, "agent", &key, raised).unwrap();
        let status = status(home.path(), "agent", &key, raised).unwrap();
        assert!(!status.budget_pending);
        assert_eq!(status.effective.per_day, Some(75.0));
        assert!(accept_current(
            home.path(),
            &user,
            "agent",
            &key,
            Some(Budget {
                per_day: Some(50.0),
                per_week: None
            })
        )
        .unwrap_err()
        .contains("changed"));
        set_current(home.path(), &user, "agent", &key, None).unwrap();
        assert!(
            !super::status(home.path(), "agent", &key, None)
                .unwrap()
                .budget_pending
        );
    }
    #[test]
    fn keep_restores_the_effective_cap_and_rejects_a_stale_request_without_ratchet_changes() {
        let home = tempfile::tempdir().unwrap();
        let user = alice();
        write_org(home.path(), 1);
        let mut definition = agent("keep-cap", AgentCli::Claude);
        let cap = |n| {
            Some(Budget {
                per_day: Some(n),
                per_week: None,
            })
        };
        definition.budget_usd = cap(50.0);
        let key = save_team_agent(home.path(), &user, definition);
        let mut current = crate::agents::service::get(home.path(), &user, &key)
            .unwrap()
            .agent;
        let revision = current.revision;
        current.budget_usd = cap(500.0);
        crate::agents::service::save(
            home.path(),
            &user,
            crate::agents::service::SaveTarget::Update {
                key: key.clone(),
                base_revision: revision,
            },
            current,
            &crate::routines::Author::agent("worker"),
        )
        .unwrap();
        keep_current(home.path(), &user, "agent", &key.to_string(), cap(500.0)).unwrap();
        assert_eq!(
            read_budget(home.path(), &user, "agent", &key.to_string()).unwrap(),
            cap(50.0)
        );
        set_current(home.path(), &user, "agent", &key.to_string(), cap(75.0)).unwrap();
        assert!(
            keep_current(home.path(), &user, "agent", &key.to_string(), cap(500.0))
                .unwrap_err()
                .contains("changed")
        );
        assert_eq!(
            status(home.path(), "agent", &key.to_string(), cap(75.0))
                .unwrap()
                .effective
                .per_day,
            Some(75.0)
        );
    }
    #[test]
    fn empty_file_cap_fields_can_be_kept_or_accepted_as_the_displayed_removal() {
        let home = tempfile::tempdir().unwrap();
        let user = alice();
        write_org(home.path(), 1);
        let mut definition = agent("empty-cap", AgentCli::Claude);
        definition.budget_usd = Some(Budget {
            per_day: Some(50.0),
            per_week: None,
        });
        let key = save_team_agent(home.path(), &user, definition);
        let clear_fields = || {
            let mut current = crate::agents::service::get(home.path(), &user, &key)
                .unwrap()
                .agent;
            let revision = current.revision;
            current.budget_usd = Some(Budget::default());
            crate::agents::service::save(
                home.path(),
                &user,
                crate::agents::service::SaveTarget::Update {
                    key: key.clone(),
                    base_revision: revision,
                },
                current,
                &crate::routines::Author::agent("worker"),
            )
            .unwrap();
        };
        clear_fields();
        keep_current(home.path(), &user, "agent", &key.to_string(), None).unwrap();
        assert_eq!(
            read_budget(home.path(), &user, "agent", &key.to_string())
                .unwrap()
                .unwrap()
                .per_day,
            Some(50.0)
        );
        clear_fields();
        accept_current(home.path(), &user, "agent", &key.to_string(), None).unwrap();
        assert_eq!(
            status(
                home.path(),
                "agent",
                &key.to_string(),
                Some(Budget::default())
            )
            .unwrap()
            .effective,
            Budget::default()
        );
    }
    #[test]
    fn pending_edit_time_is_stable_until_the_requested_values_change() {
        let (_dir, mut store) = crate::session_usage::tests::store();
        let day = chrono::Local::now().date_naive();
        let cap = |n| Budget {
            per_day: Some(n),
            per_week: None,
        };
        store.cap_status("agent", "a", cap(10.0), day, false).unwrap();
        let first = store.cap_status("agent", "a", cap(20.0), day, false).unwrap();
        assert!(first.pending_at.is_some());
        let again = store.cap_status("agent", "a", cap(20.0), day, false).unwrap();
        assert_eq!(first.pending_at, again.pending_at);
    }
}

#[cfg(test)]
mod live_identity_seams {
    use super::*;
    use crate::agents::{service, AgentKey, Placement, SaveTarget};
    use crate::commands::dashboard_launch::{DashboardLaunchState, TAG_LAUNCH_AGENT_REVISION};
    use crate::context_repo::scratch_space;
    use crate::routines::test_support::{agent, alice, save_personal_agent, write_org};
    use crate::session_usage::{tests::event, Rate, SetModelRateParams, SET_BY_AGENT, SET_BY_USER};
    use codesense_pty_core::agent_cli::AgentCli;
    use codesense_pty_core::domain::{PtySessionId, ServerId, TerminalDomain, WorkspaceKey};
    use codesense_pty_core::layout::{
        EntryState, LayoutSnapshot, Placement as TabPlacement, ServerIdentity, TabEntry,
        WorkspaceLayout,
    };
    use std::sync::Arc;

    fn private_agent(home: &Path, org: &str, budget: Option<Budget>) -> service::SavedAgent {
        let mut definition = agent("worker", AgentCli::Claude);
        definition.budget_usd = budget;
        service::save(
            home,
            &alice(),
            SaveTarget::Create(Placement::Personal(crate::routines::PersonalOwner {
                github_user_id: 1,
                username: "alice".into(),
                display_name: "Alice".into(),
                avatar_url: None,
                org: org.into(),
            })),
            definition,
            &crate::routines::Author::user("1"),
        )
        .unwrap()
    }

    fn update_budget(home: &Path, key: &AgentKey, budget: Option<Budget>) {
        let mut definition = service::read_current(home, &alice(), key).unwrap();
        let base_revision = definition.revision;
        definition.budget_usd = budget;
        service::save(
            home,
            &alice(),
            SaveTarget::Update {
                key: key.clone(),
                base_revision,
            },
            definition,
            &crate::routines::Author::agent("worker"),
        )
        .unwrap();
    }

    fn record_usage(home: &Path, key: &AgentKey, model: &str, cost: Option<f64>) {
        let mut usage = UsageStore::open(&home.join("usage.sqlite")).unwrap();
        let session = format!("usage:{key}");
        usage
            .attribute(&session, &key.org, None, &key.to_string(), "worker", None)
            .unwrap();
        let mut row = event(
            &session,
            &session,
            model,
            chrono::Utc::now().timestamp_millis(),
            crate::session_usage::event::Tokens {
                input: 1_000_000,
                ..Default::default()
            },
        );
        row.reported_cost_usd = cost;
        usage.ingest(&[row]).unwrap();
    }

    #[test]
    fn org_switch_keeps_interactive_and_headless_identity_and_clears_omission_holds() {
        use crate::commands::machines::{
            RuntimeCounter, RuntimeLayoutReply, RuntimeMemberAddress, RuntimeMemberSlot,
            RuntimeMemberTarget, RuntimeNamespace,
        };
        let home = tempfile::tempdir().unwrap();
        let org_a = write_org(home.path(), 1);
        let org_b =
            crate::context_repo::path_to_org_root(home.path(), &alice(), "other/org").unwrap();
        std::fs::create_dir_all(org_b.join(".git")).unwrap();
        for name in ["people.yml", "teams.yml"] {
            std::fs::copy(org_a.join(name), org_b.join(name)).unwrap();
        }
        std::fs::write(
            org_b.join("org.yml"),
            "slug: other/org\ndisplay_name: Other\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(crate::context_repo::path_to_team_content_dir(
            &org_b, "backend",
        ))
        .unwrap();
        let hidden = private_agent(home.path(), "acme/api", None);
        let viewed = private_agent(
            home.path(),
            "other/org",
            Some(Budget {
                per_day: Some(0.5),
                per_week: None,
            }),
        );
        record_usage(home.path(), &viewed.key, "claude-opus-5-5", Some(1.0));
        record_usage(home.path(), &hidden.key, "claude-opus-5-5", Some(1.0));
        let revision = format!("{}@{}", hidden.key, hidden.agent.revision);
        let interactive = uuid::Uuid::new_v4().to_string();
        let headless = uuid::Uuid::new_v4().to_string();
        let registry = DashboardLaunchState::new(1);
        assert!(registry.begin("headless").unwrap());
        registry.bind("headless", &headless, None).unwrap();
        registry.set_agent_revision_if_absent("headless", revision.clone());
        registry.mark_headless(
            "headless",
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let snapshot = LayoutSnapshot {
            schema: 1,
            server_id: ServerId::new("local"),
            server_name: "Local".into(),
            epoch: "test".into(),
            rev: 1,
            viewer: Default::default(),
            workspaces: [(
                WorkspaceKey::new("work"),
                WorkspaceLayout {
                    tabs: vec![TabEntry {
                        session: PtySessionId::new(&interactive),
                        name: "worker".into(),
                        role: None,
                        placement: TabPlacement::Tab,
                        state: EntryState::Live,
                        tags: [(TAG_LAUNCH_AGENT_REVISION.into(), revision)].into(),
                    }],
                    ..Default::default()
                },
            )]
            .into(),
        };
        let sessions = session_revisions(
            vec![RuntimeLayoutReply::Live {
                private_authority_epoch: None,
                target: RuntimeMemberTarget {
                    member: RuntimeMemberAddress::local(RuntimeMemberSlot::InProcess),
                    admission_revision: RuntimeCounter::new(1),
                    namespace: RuntimeNamespace {
                        server_id: "local".into(),
                        epoch: "test".into(),
                        generation: RuntimeCounter::new(1),
                    },
                },
                snapshot: Box::new(snapshot),
            }],
            registry.snapshot(),
        );
        assert_eq!(sessions.len(), 2);
        scratch_space::set_active_org(home.path(), Some("other/org"));
        assert!(!service::list(home.path(), &alice(), true)
            .unwrap()
            .iter()
            .any(|a| a.key == hidden.key));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = Arc::new(codesense_pty_core::TerminalHost::new(
            ServerIdentity::ephemeral("spend-test"),
            Arc::new(codesense_session_server::testing::RecordingSink::default()),
            runtime.handle().clone(),
        ));
        let hosts: Vec<Arc<dyn TerminalDomain>> = vec![host.clone()];
        push_session_holds(&evaluate(home.path(), &alice()), sessions.clone(), &hosts).unwrap();
        for session in [&interactive, &headless] {
            assert!(refuse_held_session(session)
                .unwrap_err()
                .contains("could not be evaluated"));
            assert!(!host.activity().inbox().wake_allowed(session));
        }
        // Ordinary reevaluation replaces both the cached and host decisions.
        for budget in [
            None,
            Some(Budget {
                per_day: Some(10.0),
                per_week: None,
            }),
        ] {
            update_budget(home.path(), &hidden.key, budget);
            let result = evaluate_sessions(home.path(), &alice(), &sessions);
            let value = result.as_ref().unwrap();
            assert!(value.agents[&hidden.key.to_string()].refusal.is_none());
            assert_eq!(
                value.agents[&hidden.key.to_string()].effective,
                budget.unwrap_or_default()
            );
            assert_eq!(value.agents[&viewed.key.to_string()].spend.state, "reached");
            push_session_holds(&result, sessions.clone(), &hosts).unwrap();
            for session in [&interactive, &headless] {
                refuse_held_session(session).unwrap();
                assert!(host.activity().inbox().wake_allowed(session));
            }
        }
        assert_eq!(
            scratch_space::active_org(home.path()).as_deref(),
            Some("other/org")
        );
    }

    #[test]
    fn hidden_live_agent_enforces_current_policy_and_pending_requests_without_freezing_launch_cap()
    {
        let home = tempfile::tempdir().unwrap();
        write_org(home.path(), 1);
        let key = save_personal_agent(home.path(), &alice(), agent("worker", AgentCli::Claude));
        let session = uuid::Uuid::new_v4().to_string();
        let launch_revision = service::read_current(home.path(), &alice(), &key)
            .unwrap()
            .revision;
        let sessions: BTreeMap<_, _> =
            [(session.clone(), Some(format!("{key}@{launch_revision}")))].into();
        scratch_space::set_active_org(home.path(), None);
        let cap = Budget {
            per_day: Some(1.0),
            per_week: Some(1.0),
        };
        update_budget(home.path(), &key, Some(cap));
        record_usage(home.path(), &key, "sample-model", None);
        let mut usage = UsageStore::open(&home.path().join("usage.sqlite")).unwrap();
        let rate = SetModelRateParams {
            model_prefix: "sample-model".into(),
            effective_from: "1970-01-01".into(),
            input_per_mtok: 2.0,
            output_per_mtok: 2.0,
            cache_read_per_mtok: None,
            long: None,
        };
        usage.request_rate(rate.clone(), SET_BY_AGENT).unwrap();
        let result = evaluate_sessions(home.path(), &alice(), &sessions);
        let status = &result.as_ref().unwrap().agents[&key.to_string()];
        assert_eq!(status.spend.state, "unpriced");
        assert_eq!(status.pending_rates, vec![rate.clone()]);
        push_session_holds(&result, sessions.clone(), &[]).unwrap();
        assert!(refuse_held_session(&session)
            .unwrap_err()
            .contains("can't price"));
        usage
            .accept_rate(&rate.model_prefix, &rate.effective_from, &rate)
            .unwrap();
        let cheaper = SetModelRateParams {
            input_per_mtok: 0.1,
            output_per_mtok: 0.1,
            ..rate
        };
        usage.request_rate(cheaper.clone(), SET_BY_AGENT).unwrap();
        for request in [
            Some(Budget {
                per_day: Some(10.0),
                per_week: Some(10.0),
            }),
            None,
        ] {
            update_budget(home.path(), &key, request);
            let result = evaluate_sessions(home.path(), &alice(), &sessions);
            let status = &result.as_ref().unwrap().agents[&key.to_string()];
            assert!(status.budget_pending);
            assert_eq!(status.effective, cap);
            assert_eq!(status.spend.today, 2.0);
            assert_eq!(status.spend.state, "reached");
            assert_eq!(status.pending_rates, vec![cheaper.clone()]);
            assert!(status
                .alerts
                .iter()
                .any(|a| a.level == 100 && a.detail.contains("local week")));
            push_session_holds(&result, sessions.clone(), &[]).unwrap();
            assert!(refuse_held_session(&session)
                .unwrap_err()
                .contains("local day"));
        }
        // Pending requests alone do not refuse an agent whose measured spend is below its accepted cap.
        usage
            .set_rate_and_reprice(
                "sample-model",
                "1970-01-01",
                Rate::flat(0.1, 0.1),
                SET_BY_USER,
            )
            .unwrap();
        let result = evaluate_sessions(home.path(), &alice(), &sessions);
        let status = &result.as_ref().unwrap().agents[&key.to_string()];
        assert!(status.budget_pending);
        assert!(status.refusal.is_none());
        push_session_holds(&result, sessions.clone(), &[]).unwrap();
        refuse_held_session(&session).unwrap();

        let missing = AgentKey::personal(&alice(), "acme/api", "missing");
        let sessions = [(session.clone(), Some(format!("{missing}@1")))].into();
        let result = evaluate_sessions(home.path(), &alice(), &sessions);
        assert!(!result
            .as_ref()
            .unwrap()
            .agents
            .contains_key(&missing.to_string()));
        push_session_holds(&result, sessions.clone(), &[]).unwrap();
        assert!(refuse_held_session(&session)
            .unwrap_err()
            .contains("could not be evaluated"));
        push_session_holds(&Err("usage unavailable".into()), sessions, &[]).unwrap();
        assert!(refuse_held_session(&session)
            .unwrap_err()
            .contains("spend unavailable"));
    }
}
