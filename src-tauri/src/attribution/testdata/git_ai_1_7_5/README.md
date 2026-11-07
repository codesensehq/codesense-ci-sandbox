# git-ai 1.7.5 fixture

Real output of `git-ai` v1.7.5 (`git-ai-macos-arm64`, sha256
`f1c0ce62ef17fc259e830463c4c3e186e404a2d98ec1b127e79d1bbd50e36ac2`), run only under a
sandbox `HOME` with telemetry off, using scripted hook payloads.

- `c0/`, `c1/`, `c2/`: the working tree at each of three commits. `cN.msg` is its message.
- `cN.note`: the exact bytes of `git notes --ref=ai show` for that commit. `c0` has none
  (git-ai was not observing); `c1` holds a Claude range (`a.txt`, which also carries a human
  line git-ai credited to Claude: the known limit) and a Codex range (`b.txt`); `c2` holds a
  `known_human` line (`d.txt` line 3).
- `prompt/` and `prompt.note`: a Claude checkpoint whose transcript contained
  `PROMPTSECRET` and `ASSISTANTSECRET`, with the managed config (`exclude_prompts_in_repositories=["*"]`,
  `prompt_storage=local`). The note holds neither string. With the config reverted to
  `prompt_storage=notes` and no exclusion the note was byte-for-byte the same shape, so the
  1.7.5 `authorship/3.0.0` note carries no prompt text either way.

The tests rebuild the repository from these files and attach each note with
`git notes --ref=ai add -F`.
