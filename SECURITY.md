# Security policy

## Threat model

flpdf treats every input PDF as attacker-controlled. What that means in
practice — the guarantees we make for arbitrary input, what counts as a
vulnerability, and what is explicitly out of scope — is defined in
[docs/threat-model.md](docs/threat-model.md). Please read it before filing:
it tells you whether what you found is a security bug (e.g. a panic, abort,
or hang on malformed input) or expected behavior (e.g. high memory use on a
compression bomb, which we document as out of scope).

## Supported versions

flpdf is pre-1.0. Security fixes land on `main` and are included in the next
release; older releases are not patched.

## Reporting a vulnerability

Please report vulnerabilities privately via GitHub:
**Security → Report a vulnerability** on this repository
(GitHub private vulnerability reporting). Do not open a public issue for
anything you believe is exploitable before a fix is available.

Include if you can:

- a minimal reproducing PDF (or a description of how to construct one),
- the flpdf version / commit and the command or API call used,
- what happened (panic message, hang, sanitizer output, …).

Reports that fall under "What we consider a vulnerability" in the threat
model are prioritized; everything else is welcome as a regular GitHub issue.
