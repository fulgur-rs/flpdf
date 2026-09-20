#!/bin/bash
# D16 probe (flpdf-3yn9.48.159): does the CLI-owned `--copy-encryption` donor
# boundary (`Pdf::writer_copy_encryption_source` called straight from
# flpdf-cli) diverge from qpdf 11.9.0's `QPDFJob::setWriterOptions` ->
# `processFile` -> `QPDFWriter::copyEncryptionParameters` route?
#
# Usage: d16_probe.sh <workdir> [path-to-flpdf]
#
# `--static-id --static-aes-iv` are mandatory: without the AES IV fixed, every
# AES donor produces a fresh IV per run and every byte compare is a false
# positive.
#
# Result on 2026-09-18 (qpdf 11.9.0, flpdf @ da153c06a, --features
# qpdf-zlib-compat):
#   byte-identical: d-plain d-rc4-40 d-rc4-128 d-aes-128 d-aes-256
#                   d-v1-len048 d-v2-nolength + owner-password opens
#   DIVERGENT (qpdf rc=0 + output vs flpdf rc=2 + no output):
#                   d-v2-len032 d-v2-len240 d-v4-len040 d-v5-len128
#                   (crates/flpdf/src/writer.rs:2747-2800 rejects a /Length
#                    that disagrees with the donor's authenticated key length;
#                    qpdf re-derives the key at /Length/8 instead)
#   DIVERGENT (message framing only, both rc=2):
#                   missing-donor wrong-pw no-pw donor-is-dir donor-not-pdf
#   DIVERGENT (unsupported-handler message text, both rc=2 -- added
#   2026-09-20, flpdf-rxtdr, flpdf @ 4f20a5953; unlike the message-framing
#   set above, this one is not yet byte-identical):
#                   d-v3 (qpdf: "Unsupported /R or /V in encryption
#                   dictionary; R = 3 (max 6), V = 3 (max 5)"; flpdf:
#                   "unsupported encryption handler: filter=Standard, V=3,
#                   R=3, CFM=None". Both correctly reject the donor -- qpdf's
#                   own encryption-handler registry has no V=3/R=3 entry
#                   either -- so this is wording only, tracked separately
#                   from the fixed message-framing set since it has not been
#                   matched to qpdf's text yet.)
#   DIVERGENT but reader-level, reproduces without --copy-encryption:
#                   d-v4-r3 d-v4-r5 d-v2-r4
#   DIVERGENT with --password-is-hex-key (hexkey-*): qpdf re-derives the
#                   output key from getPaddedUserPassword(), flpdf reuses the
#                   supplied file key, so even the exact-length case differs in
#                   bytes while both exit 0 -- and qpdf's own output then fails
#                   its own --check.  Off-length keys make flpdf exit 2, from
#                   the CLI predicate (main.rs:5470-5484) on the donor path and
#                   from writer.rs:2747-2800 on the primary-input path.
set -eu
P="$(realpath "${1:?workdir}")"
FL="${2:-flpdf}"
case "$FL" in */*) FL="$(realpath "$FL")" ;; esac
export FLPDF_STATIC_ID_QUIET=1
rm -rf "$P/fix" "$P/q" "$P/f"; mkdir -p "$P/fix" "$P/q" "$P/f"
python3 "$(dirname "$0")/make_fixtures.py" "$P/fix" >/dev/null
cd "$P/fix" || exit 1
cp e16-unshared.pdf base.pdf
cp e16-unshared.pdf d-plain.pdf
/usr/bin/qpdf --allow-weak-crypto --static-id --encrypt --user-password=u --owner-password=o --bits=40  -- base.pdf d-rc4-40.pdf
/usr/bin/qpdf --allow-weak-crypto --static-id --encrypt --user-password=u --owner-password=o --bits=128 --use-aes=n -- base.pdf d-rc4-128.pdf
/usr/bin/qpdf --static-id --encrypt --user-password=u --owner-password=o --bits=128 --use-aes=y -- base.pdf d-aes-128.pdf
/usr/bin/qpdf --static-id --encrypt --user-password=u --owner-password=o --bits=256 -- base.pdf d-aes-256.pdf
python3 - <<'PY'
def patch(src, dst, old, new):
    d = open(src, 'rb').read()
    assert len(old) == len(new) and d.count(old) == 1, (src, old)
    open(dst, 'wb').write(d.replace(old, new))
# Same-width in-place edits keep every xref offset valid.  The /Encrypt
# dictionary is never itself encrypted, so patching it is safe.
patch('d-rc4-40.pdf',  'd-v1-len048.pdf',   b'/Length 40 /O',  b'/Length 48 /O')
patch('d-rc4-128.pdf', 'd-v2-len240.pdf',   b'/Length 128 /O', b'/Length 240 /O')
patch('d-rc4-128.pdf', 'd-v2-len032.pdf',   b'/Length 128 /O', b'/Length 032 /O')
patch('d-rc4-128.pdf', 'd-v2-nolength.pdf', b'/Length 128 /O', b'/Xength 128 /O')
patch('d-aes-128.pdf', 'd-v4-len040.pdf',   b'/Length 128 /O', b'/Length 040 /O')
patch('d-aes-256.pdf', 'd-v5-len128.pdf',   b'/Length 256 /O', b'/Length 128 /O')
patch('d-rc4-128.pdf', 'd-v3.pdf',          b'/V 2 >>',        b'/V 3 >>')
patch('d-aes-128.pdf', 'd-v4-r3.pdf',       b'/R 4 /StmF',     b'/R 3 /StmF')
patch('d-aes-128.pdf', 'd-v4-r5.pdf',       b'/R 4 /StmF',     b'/R 5 /StmF')
patch('d-rc4-128.pdf', 'd-v2-r4.pdf',       b'/R 3 /U',        b'/R 4 /U')
PY
# Setup (fixture generation above) must not fail silently into a partial
# fixture set that then produces spurious DIFFs below -- `set -e` covers it.
# The probe cells themselves intentionally exercise failure paths (wrong
# password, missing donor, ...), so turn -e back off before running them.
set +e
run() { tag="$1"; shift
  /usr/bin/qpdf "${@//@OUT@/$P/q/$tag.pdf}" >"$P/q/$tag.out" 2>"$P/q/$tag.err"; echo $? >"$P/q/$tag.rc"
  "$FL"         "${@//@OUT@/$P/f/$tag.pdf}" >"$P/f/$tag.out" 2>"$P/f/$tag.err"; echo $? >"$P/f/$tag.rc"
}
for d in d-plain d-rc4-40 d-rc4-128 d-aes-128 d-aes-256 d-v1-len048 d-v3 \
         d-v2-len240 d-v2-len032 d-v2-nolength d-v4-len040 d-v5-len128 \
         d-v4-r3 d-v4-r5 d-v2-r4; do
  run "$d" --static-id --static-aes-iv --allow-weak-crypto base.pdf @OUT@ \
      --copy-encryption="$d.pdf" --encryption-file-password=u
done
for d in d-rc4-40 d-rc4-128 d-aes-128 d-aes-256; do
  run "ownerpw-$d" --static-id --static-aes-iv --allow-weak-crypto base.pdf @OUT@ \
      --copy-encryption="$d.pdf" --encryption-file-password=o
done
run "missing-donor"  --static-id base.pdf @OUT@ --copy-encryption=no-such-file.pdf
run "wrong-pw"       --static-id base.pdf @OUT@ --copy-encryption=d-aes-256.pdf --encryption-file-password=WRONG
run "no-pw"          --static-id base.pdf @OUT@ --copy-encryption=d-aes-256.pdf
run "donor-is-dir"   --static-id base.pdf @OUT@ --copy-encryption=.
# Control: the same donors as the PRIMARY input, no --copy-encryption at all.
# qpdf routes preserve-encryption through copyEncryptionParameters too
# (QPDFWriter.cc:2100), so these share the canonical builder.
for d in d-v2-len032 d-v2-len240 d-v4-len040 d-v5-len128 d-v4-r3 d-v4-r5 d-v2-r4; do
  run "primary-$d" --static-id --static-aes-iv --allow-weak-crypto --password=u "$d.pdf" @OUT@
done
# --password-is-hex-key: the only way to give the reader a file key whose
# length disagrees with the /Encrypt dictionary, so it reaches both the CLI
# predicate and the canonical builder's key-length check.
KEY=$(/usr/bin/qpdf --show-encryption --show-encryption-key --password=u d-rc4-128.pdf 2>/dev/null \
      | sed -n 's/^Encryption key = //p')
for spec in "exact:$KEY" "long:${KEY}0011223344" "short:6836c004cc15007d" "empty:"; do
  name="${spec%%:*}"; key="${spec#*:}"
  run "hexkey-$name" --static-id --static-aes-iv --allow-weak-crypto --password-is-hex-key \
      base.pdf @OUT@ --copy-encryption=d-rc4-128.pdf --encryption-file-password="$key"
  run "hexkey-primary-$name" --static-id --static-aes-iv --allow-weak-crypto \
      --password-is-hex-key --password="$key" d-rc4-128.pdf @OUT@
done
for x in "$P"/f/*.out "$P"/f/*.err; do sed -i "s/^flpdf:/qpdf:/; s#$P/f/#OUTDIR/#g" "$x"; done
for x in "$P"/q/*.out "$P"/q/*.err; do sed -i "s#$P/q/#OUTDIR/#g" "$x"; done
cd "$P" || exit 1
rc=0
for x in q/*; do n=$(basename "$x"); diff -q "q/$n" "f/$n" >/dev/null 2>&1 || { echo "DIFF $n"; rc=1; }; done
comm -3 <(ls q) <(ls f) | sed 's/^/ONE-SIDED /' | grep . && rc=1
exit $rc
