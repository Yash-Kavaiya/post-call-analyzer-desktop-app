# Security Policy

## Reporting a vulnerability

**Do not open a public issue.**

Report privately through GitHub:
[**Report a vulnerability**](https://github.com/Yash-Kavaiya/call-analyzer/security/advisories/new)

If that is not available to you, email **yash.kavaiya3@gmail.com** with
`SECURITY` in the subject.

Please include what you have: what the flaw is, how to reach it, what an
attacker gets from it, and the version you tested. A rough report sent
promptly is worth more than a polished one sent next month.

### What to expect

| | |
|---|---|
| Acknowledgement | Within 3 working days |
| Initial assessment | Within 7 days |
| Fix or mitigation plan | Depends on severity; you will be told which |
| Credit | Offered in the advisory and release notes, declined on request |

This is a small project maintained by one person. Those are targets held in
good faith, not a contractual SLA.

---

## What matters most here

Call Analyzer processes contact-center recordings. That means the data in
scope is not incidental — it is call audio, transcripts of what customers
said, and the personal information inside them. The following are treated as
the highest severity, regardless of how difficult they are to exploit:

- **Credential exposure.** API keys are held in the Windows Credential
  Manager (`ca-llm::secrets`) and must never reach the database, a log line,
  the UI, or an outbound request to anywhere but the provider they belong to.
- **PII leaking past redaction.** `ca-privacy` redacts before anything is
  sent to a cloud LLM. A bypass — an encoding it does not normalise, a path
  that skips it — means customer data leaving the machine.
- **Recordings leaving the machine unexpectedly.** Transcription is local by
  design. Any path that uploads audio is a defect unless it is the documented
  cloud-scoring flow operating on redacted text.
- **Code execution.** Through a crafted audio file reaching the decoders, a
  malicious model or whisper binary path, or the Tauri command surface.
- **Tauri capability escalation.** `capabilities/default.json` decides what
  the frontend can reach. Widening it is a security change.

Lower severity, still worth reporting: denial of service through malformed
input, information disclosure limited to the local machine, and dependency
advisories that are not reachable from any code path this product executes.

---

## Out of scope

- Anything requiring an attacker who already has administrator access to the
  machine, or physical access to an unlocked session.
- The security of a third-party LLM provider you have configured with your
  own key.
- Vulnerabilities in whisper.cpp itself — report those upstream; tell us if
  the way this project invokes it makes them worse.
- Findings from an automated scanner with no demonstrated path to impact.

---

## For contributors

The checks that back this policy run automatically — see
[docs/CI_CD.md](docs/CI_CD.md):

- **Secret scanning** over full history on every push and weekly
- **`cargo-deny`** for advisories, licences, banned crates, and sources
- **`npm audit`**, blocking on dependencies that ship to users
- **Dependency review** on every pull request
- **CodeQL** over the TypeScript UI
- **actionlint** over the workflows themselves

Three habits matter more than any of those:

1. **Never log a secret or a transcript.** `tracing` output ends up in files
   users send to support.
2. **Redact before the network, not after.** If a code path can reach a
   provider, trace it back to a redaction step.
3. **Treat `capabilities/default.json` and `ca-privacy` as review-heavy.**
   They are listed in `CODEOWNERS` for that reason.

If you find something while working on a change, do not fix it quietly in a
large PR. Report it through the process above so it can be tracked, fixed,
and disclosed properly.

---

## Supported versions

The project is pre-1.0. Only the latest release receives fixes.

| Version | Supported |
|---|---|
| Latest release | Yes |
| Anything earlier | No — upgrade |

---

## Verifying a release

Every release carries SHA-256 checksums, a CycloneDX SBOM, and a signed
build-provenance attestation:

```powershell
(Get-FileHash .\<file> -Algorithm SHA256).Hash.ToLower()   # compare to SHA256SUMS
gh attestation verify .\<file> --repo Yash-Kavaiya/call-analyzer
```

The attestation proves the bytes you downloaded were produced by the release
workflow in this repository, from a specific commit. If it does not verify,
do not run the installer — report it.
