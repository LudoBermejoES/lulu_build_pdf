#!/usr/bin/env bash
# Regenerates the binary PDF fixtures in this directory. Requires qpdf.
#
# lopdf 0.44's writer has a bug where an /Encrypt trailer entry is dropped on
# save (see crates/lulu-prep/src/pdf.rs doc comments), so these encrypted
# fixtures are produced by qpdf instead of by our own code, and only ever
# *read* by lopdf in tests — never round-tripped through lopdf's writer.
set -euo pipefail
cd "$(dirname "$0")"

# A minimal single-page, unencrypted PDF at exactly the 6x9in-with-bleed
# page size (450x666 pt) our geometry tests expect, with a hand-computed
# xref table (byte-exact, no repair needed — qpdf --check confirms this).
python3 - <<'PYEOF'
buf = b"%PDF-1.4\n"
offsets = [0]

obj1 = b"1 0 obj\n<</Type/Pages/Kids[2 0 R]/Count 1>>\nendobj\n"
obj2 = b"2 0 obj\n<</Type/Page/Parent 1 0 R/Resources<<>>/MediaBox[0 0 450 666]>>\nendobj\n"
obj3 = b"3 0 obj\n<</Type/Catalog/Pages 1 0 R>>\nendobj\n"

for obj in (obj1, obj2, obj3):
    offsets.append(len(buf))
    buf += obj

xref_offset = len(buf)
xref = b"xref\n0 4\n0000000000 65535 f \n"
for off in offsets[1:]:
    xref += f"{off:010d} 00000 n \n".encode()
buf += xref
buf += b"trailer\n<</Root 3 0 R/Size 4>>\nstartxref\n" + str(xref_offset).encode() + b"\n%%EOF\n"

open("plain.pdf", "wb").write(buf)
PYEOF

qpdf --encrypt "" ownersecret 256 -- plain.pdf encrypted_empty_password.pdf
qpdf --encrypt "realsecret" ownersecret 256 -- plain.pdf encrypted_real_password.pdf
rm plain.pdf
echo "Regenerated encrypted_empty_password.pdf and encrypted_real_password.pdf"
