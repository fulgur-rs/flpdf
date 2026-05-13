# Encrypted PDF Fixtures

This directory contains small synthetic encrypted PDFs generated from
`tests/fixtures/minimal.pdf` with qpdf 11.9.0. They contain only trivial test
content and no real-world data.

Regenerate fixtures with:

```sh
tests/fixtures/encrypted/generate.sh --generate
```

Verify the committed fixtures with:

```sh
tests/fixtures/encrypted/generate.sh --check
```

`--check` decrypts each fixture with `qpdf --decrypt --static-id` and compares
the SHA-256 digest below. The digest is the plaintext PDF emitted by qpdf after
decryption, not the encrypted fixture bytes.

| Fixture | Coverage | User password | Owner password | Expected plaintext SHA-256 |
| --- | --- | --- | --- | --- |
| `v1-rc4-40-r2.pdf` | V=1, R=2, RC4-40 | `user-v1` | `owner-v1` | `a3668e477aa1c1e138f9ffa965db3d81cb6abaceeb8b7269d8e8622d29df753d` |
| `v2-rc4-128-r3.pdf` | V=2, R=3, RC4-128 | `user-v2` | `owner-v2` | `a3668e477aa1c1e138f9ffa965db3d81cb6abaceeb8b7269d8e8622d29df753d` |
| `v4-rc4-128-r4.pdf` | V=4, R=4, RC4-128 | `user-v4-rc4` | `owner-v4-rc4` | `a3668e477aa1c1e138f9ffa965db3d81cb6abaceeb8b7269d8e8622d29df753d` |
| `v4-aes-128-r4.pdf` | V=4, R=4, AES-128 | `user-v4-aes` | `owner-v4-aes` | `a3668e477aa1c1e138f9ffa965db3d81cb6abaceeb8b7269d8e8622d29df753d` |
| `v5-aes-256-r5.pdf` | V=5, R=5, AES-256 | `user-v5-r5` | `owner-v5-r5` | `ee894a875f95c6e53451fa3bb84683af3adbb329dba4b58cef502054a3a5d518` |
| `v5-aes-256-r6.pdf` | V=5, R=6, AES-256 | `user-v5-r6` | `owner-v5-r6` | `f54d7aa9e6150ce3dc675c615ca571f2c8a924e0853e5ad215674951655ed42a` |

The generator uses these qpdf commands, all with `--static-id`:

```sh
qpdf --static-id --allow-weak-crypto --encrypt user-v1 owner-v1 40 -- tests/fixtures/minimal.pdf v1-rc4-40-r2.pdf
qpdf --static-id --allow-weak-crypto --encrypt user-v2 owner-v2 128 --use-aes=n -- tests/fixtures/minimal.pdf v2-rc4-128-r3.pdf
qpdf --static-id --allow-weak-crypto --encrypt user-v4-rc4 owner-v4-rc4 128 --use-aes=n --force-V4 -- tests/fixtures/minimal.pdf v4-rc4-128-r4.pdf
qpdf --static-id --allow-weak-crypto --encrypt user-v4-aes owner-v4-aes 128 --use-aes=y -- tests/fixtures/minimal.pdf v4-aes-128-r4.pdf
qpdf --static-id --encrypt user-v5-r5 owner-v5-r5 256 --force-R5 -- tests/fixtures/minimal.pdf v5-aes-256-r5.pdf
qpdf --static-id --encrypt user-v5-r6 owner-v5-r6 256 -- tests/fixtures/minimal.pdf v5-aes-256-r6.pdf
```

`qpdf --deterministic-id` cannot be combined with encryption in qpdf 11.9.0, so
the generator uses `--static-id` instead. R=5/R=6 encrypted fixture bytes may
still vary across qpdf versions, but the plaintext digest check is stable for the
committed fixtures.
