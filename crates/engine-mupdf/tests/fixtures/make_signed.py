#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
"""Writes the signed fixtures of Plan 0009 Phase 3 (row BC): the PDF around a signature is written
here, by hand, and the signature is made by OpenSSL's `cms`, an implementation that shares no code
with PODEFA. Run from this folder with `openssl` on the PATH. The keys are thrown away; only the
root's certificate is kept (`signed_root.pem`), since a test trusts it by name.

    signed_rsa.pdf        RSA 2048, SHA-256, leaf < intermediate < root, the chain embedded
    signed_ec.pdf         ECDSA P-256, SHA-256, same chain
    signed_pss.pdf        RSA-PSS, SHA-384, same chain
    signed_selfsigned.pdf RSA 2048, a self-signed certificate no one trusts
    signed_docmdp1.pdf    as signed_rsa, certifying: DocMDP level 1, no changes allowed
    signed_docmdp3.pdf    as signed_rsa, certifying: DocMDP level 3, annotations allowed
    signed_early.pdf      as signed_rsa, dated before its certificates begin
    signed_forged_mid.pdf another key's leaf with the real intermediate beside it
    signed_gap.pdf        a byte range that leaves out /Reason too, signed as such
    signed_shared.pdf     level 3, with a Square whose appearance the page uses as well
    signed_forged.pdf     a chain of another key under the same names as the real one's
    signed_fieldmdp.pdf   as signed_rsa, with text fields Locked and Free; FieldMDP locks Locked
"""

import os
import shutil
import subprocess
import tempfile

ROOM = 8192  # bytes of signature room, twice this in hex digits

CONFIG = """
[req]
distinguished_name = dn
prompt = no
utf8 = yes
string_mask = utf8only
[dn]
[ca]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign,cRLSign
[leaf]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature,nonRepudiation
"""


def run(*args, cwd):
    subprocess.run(["openssl", *args], check=True, cwd=cwd, capture_output=True)


def rsa_key(name, cwd):
    run("genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", name, cwd=cwd)


def ec_key(name, cwd):
    run("genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out", name, cwd=cwd)


def config(subject, cwd):
    """The subject goes in the configuration: a command line cannot carry UTF-8 on Windows."""
    organization, name = subject
    with open(os.path.join(cwd, "c.cnf"), "w", encoding="utf-8") as f:
        f.write(CONFIG.replace("[dn]", "[dn]" + chr(10) + f"O = {organization}" + chr(10) + f"CN = {name}"))


def self_signed(key, subject, out, section, cwd):
    config(subject, cwd)
    run("req", "-x509", "-new", "-utf8", "-config", "c.cnf", "-key", key,
        "-days", "10950", "-sha256", "-extensions", section, "-out", out, cwd=cwd)


def issued(key, subject, issuer, issuer_key, out, section, cwd):
    config(subject, cwd)
    run("req", "-new", "-utf8", "-config", "c.cnf", "-key", key, "-out", "r.csr", cwd=cwd)
    run("x509", "-req", "-in", "r.csr", "-CA", issuer, "-CAkey", issuer_key, "-CAcreateserial",
        "-days", "10950", "-sha256", "-extfile", "c.cnf", "-extensions", section, "-out", out, cwd=cwd)


# After the certificates begin, whenever they are made, and well within their 30 years.
DATE = b"D:20300101120000+03'00'"


def document(docmdp, date):
    """The objects of a one-page document with one signature field; object 6 is the signature."""
    reference = ""
    perms = ""
    fields = docmdp == "field"
    shared = docmdp == "shared"
    if shared:
        docmdp = 3
    if fields:
        reference = (
            "/Reference [<< /Type /SigRef /TransformMethod /FieldMDP /TransformParams "
            "<< /Type /TransformParams /Action /Include /Fields [(Locked)] /V /1.2 >> >>] "
        )
    elif docmdp:
        reference = (
            f"/Reference [<< /Type /SigRef /TransformMethod /DocMDP "
            f"/TransformParams << /Type /TransformParams /P {docmdp} /V /1.2 >> >>] "
        )
        perms = "/Perms << /DocMDP 6 0 R >> "
    stream = b"BT /F1 24 Tf 72 700 Td (SIGNED-WORDS) Tj ET"
    placeholder = b"/ByteRange [0000000000 0000000000 0000000000 0000000000]"
    contents = b"/Contents <" + b"0" * (ROOM * 2) + b">"
    extra = " 7 0 R 8 0 R" if fields else ""
    page_extra = " 7 0 R" if shared else extra
    xobject = b"/XObject << /X 8 0 R >> " if shared else b""
    objects = [
        f"<< /Type /Catalog /Pages 2 0 R {perms}/AcroForm << /Fields [5 0 R{extra}] /SigFlags 3 >> >>".encode(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R "
        b"/Resources << %s/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> "
        b"/Annots [5 0 R%s] >>" % (xobject, page_extra.encode()),
        b"<< /Length %d >>\nstream\n%s\nendstream" % (len(stream), stream),
        b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Signature1) /F 4 /Rect [72 600 272 660] "
        b"/P 3 0 R /V 6 0 R >>",
        b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached "
        + placeholder + b" " + contents + b" "
        + reference.encode()
        + b"/M (" + date + b") /Reason (Approved) /Location (Ankara) /Name (Name In The Dictionary) >>",
    ]
    if fields:
        for name, y in ((b"Locked", 500), (b"Free", 450)):
            objects.append(
                b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (%s) /F 4 /Rect [72 %d 272 %d] /P 3 0 R "
                b"/DA (/Helv 12 Tf 0 g) /V (first) >>" % (name, y, y + 30)
            )
    if shared:
        objects.append(
            b"<< /Type /Annot /Subtype /Square /F 4 /Rect [72 400 172 450] /P 3 0 R /AP << /N 8 0 R >> >>"
        )
        drawing = b"0 0 100 50 re S"
        objects.append(
            b"<< /Type /XObject /Subtype /Form /BBox [0 0 100 50] /Length %d >>\nstream\n%s\nendstream"
            % (len(drawing), drawing)
        )
    return objects


def pdf(objects):
    out = b"%PDF-1.7\n"
    offsets = []
    for i, body in enumerate(objects):
        offsets.append(len(out))
        out += b"%d 0 obj\n%s\nendobj\n" % (i + 1, body)
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objects) + 1)
    for offset in offsets:
        out += b"%010d 00000 n \n" % offset
    out += (
        b"trailer\n<< /Size %d /Root 1 0 R /ID [<0123456789abcdef0123456789abcdef> "
        b"<0123456789abcdef0123456789abcdef>] >>\nstartxref\n%d\n%%%%EOF\n" % (len(objects) + 1, xref)
    )
    return out


def sign(out_name, key, cert, chain, docmdp, extra, cwd, date=DATE, wide=False):
    data = pdf(document(docmdp, date))
    start = data.index(b"/Contents <") + len(b"/Contents ")
    end = data.index(b">", start) + 1
    if wide:
        # The range skips the hex string and the words after it: they are not signed.
        end_wide = data.index(b"/Reason", end) + len(b"/Reason (Approved)")
    else:
        end_wide = end
    ranges = b"/ByteRange [%010d %010d %010d %010d]" % (0, start, end_wide, len(data) - end_wide)
    placeholder = b"/ByteRange [0000000000 0000000000 0000000000 0000000000]"
    assert len(ranges) == len(placeholder)
    data = data.replace(placeholder, ranges)
    with open(os.path.join(cwd, "signed.bin"), "wb") as f:
        f.write(data[:start] + data[end_wide:])
    args = ["cms", "-sign", "-binary", "-in", "signed.bin", "-signer", cert, "-inkey", key,
            "-outform", "DER", "-out", "sig.der"]
    if chain:
        args += ["-certfile", chain]
    run(*args, *extra, cwd=cwd)
    with open(os.path.join(cwd, "sig.der"), "rb") as f:
        signature = f.read().hex().encode()
    assert len(signature) <= ROOM * 2
    data = data[:start + 1] + signature + b"0" * (ROOM * 2 - len(signature)) + data[end - 1:]
    with open(out_name, "wb") as f:
        f.write(data)


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    with tempfile.TemporaryDirectory() as cwd:
        rsa_key("root.key", cwd)
        self_signed("root.key", ("PODEFA Tests", "PODEFA Test Root"), "root.pem", "ca", cwd)
        rsa_key("mid.key", cwd)
        issued("mid.key", ("PODEFA Tests", "PODEFA Test Intermediate"), "root.pem", "root.key",
               "mid.pem", "ca", cwd)
        rsa_key("rsa.key", cwd)
        issued("rsa.key", ("PODEFA Tests", "Şule Çağlar Öztürk"), "mid.pem", "mid.key", "rsa.pem", "leaf", cwd)
        ec_key("ec.key", cwd)
        issued("ec.key", ("PODEFA Tests", "Elif Eğri"), "mid.pem", "mid.key", "ec.pem", "leaf", cwd)
        rsa_key("self.key", cwd)
        self_signed("self.key", ("PODEFA Tests", "Nobody Knows Me"), "self.pem", "leaf", cwd)

        path = lambda name: os.path.join(here, name)
        sign(path("signed_rsa.pdf"), "rsa.key", "rsa.pem", "mid.pem", 0, ["-md", "sha256"], cwd)
        sign(path("signed_ec.pdf"), "ec.key", "ec.pem", "mid.pem", 0, ["-md", "sha256"], cwd)
        sign(path("signed_pss.pdf"), "rsa.key", "rsa.pem", "mid.pem", 0,
             ["-md", "sha384", "-keyopt", "rsa_padding_mode:pss"], cwd)
        sign(path("signed_selfsigned.pdf"), "self.key", "self.pem", None, 0, ["-md", "sha256"], cwd)
        sign(path("signed_docmdp1.pdf"), "rsa.key", "rsa.pem", "mid.pem", 1, ["-md", "sha256"], cwd)
        sign(path("signed_docmdp3.pdf"), "rsa.key", "rsa.pem", "mid.pem", 3, ["-md", "sha256"], cwd)
        sign(path("signed_early.pdf"), "rsa.key", "rsa.pem", "mid.pem", 0, ["-md", "sha256"], cwd,
             date=b"D:20000101120000+03'00'")
        # The same names, other keys: what a chain followed by name alone would trust.
        rsa_key("fake_root.key", cwd)
        self_signed("fake_root.key", ("PODEFA Tests", "PODEFA Test Root"), "fake_root.pem", "ca", cwd)
        rsa_key("fake_mid.key", cwd)
        issued("fake_mid.key", ("PODEFA Tests", "PODEFA Test Intermediate"), "fake_root.pem",
               "fake_root.key", "fake_mid.pem", "ca", cwd)
        rsa_key("fake.key", cwd)
        issued("fake.key", ("PODEFA Tests", "Şule Çağlar Öztürk"), "fake_mid.pem", "fake_mid.key",
               "fake.pem", "leaf", cwd)
        sign(path("signed_forged.pdf"), "fake.key", "fake.pem", "fake_mid.pem", 0, ["-md", "sha256"], cwd)
        sign(path("signed_forged_mid.pdf"), "fake.key", "fake.pem", "mid.pem", 0, ["-md", "sha256"], cwd)
        sign(path("signed_gap.pdf"), "rsa.key", "rsa.pem", "mid.pem", 0, ["-md", "sha256"], cwd, wide=True)
        sign(path("signed_shared.pdf"), "rsa.key", "rsa.pem", "mid.pem", "shared", ["-md", "sha256"], cwd)
        sign(path("signed_fieldmdp.pdf"), "rsa.key", "rsa.pem", "mid.pem", "field", ["-md", "sha256"], cwd)
        shutil.copy(os.path.join(cwd, "root.pem"), path("signed_root.pem"))


if __name__ == "__main__":
    main()
