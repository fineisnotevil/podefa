#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
"""Writes the key files of Plan 0009 Phase 4 (row BD): test identities that PODEFA signs with,
made by OpenSSL's `pkcs12`, which shares no code with PODEFA. Run from this folder with `openssl`
(3.x, with its legacy provider) on the PATH. THESE KEYS ARE FOR TESTS ONLY: they are published
here, so nothing they sign means anything. Only the root's certificate is kept apart
(`signer_root.pem`), since a test trusts it by name; the root's key is thrown away.

    signer_rsa.p12     RSA 2048 leaf and its intermediate; AES-256 (PBES2), SHA-256 MAC;
                       password "Şifre 1"
    signer_legacy.p12  the same identity the way OpenSSL 1.x and older Windows wrote it: RC2-40
                       certificates, 3DES key, SHA-1 MAC; password "eski"
    signer_ec.p12      ECDSA P-256 leaf and the intermediate; AES-256; password "Şifre 1"
    signer_p384.p12    ECDSA P-384 leaf and the intermediate; AES-256; password "Şifre 1"
    signer_empty.p12   the RSA identity with an empty password
"""

import os
import shutil
import subprocess
import tempfile

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

PASSWORD = "Şifre 1"


def run(*args, cwd):
    subprocess.run(["openssl", *args], check=True, cwd=cwd, capture_output=True)


def key(name, algorithm, option, cwd):
    run("genpkey", "-algorithm", algorithm, "-pkeyopt", option, "-out", name, cwd=cwd)


def config(subject, cwd):
    """The subject goes in the configuration: a command line cannot carry UTF-8 on Windows."""
    organization, name = subject
    with open(os.path.join(cwd, "c.cnf"), "w", encoding="utf-8") as f:
        f.write(CONFIG.replace("[dn]", "[dn]" + chr(10) + f"O = {organization}" + chr(10) + f"CN = {name}"))


def self_signed(key, subject, out, cwd):
    config(subject, cwd)
    run("req", "-x509", "-new", "-utf8", "-config", "c.cnf", "-key", key,
        "-days", "10950", "-sha256", "-extensions", "ca", "-out", out, cwd=cwd)


def issued(key, subject, issuer, issuer_key, out, section, cwd):
    config(subject, cwd)
    run("req", "-new", "-utf8", "-config", "c.cnf", "-key", key, "-out", "r.csr", cwd=cwd)
    run("x509", "-req", "-in", "r.csr", "-CA", issuer, "-CAkey", issuer_key, "-CAcreateserial",
        "-days", "10950", "-sha256", "-extfile", "c.cnf", "-extensions", section, "-out", out, cwd=cwd)


def p12(out, key, cert, password, extra, cwd):
    """The password goes through a file: a command line cannot carry UTF-8 on Windows."""
    with open(os.path.join(cwd, "pw.txt"), "w", encoding="utf-8", newline="") as f:
        f.write(password + chr(10))
    run("pkcs12", "-export", "-inkey", key, "-in", cert, "-certfile", "mid.pem",
        "-passout", "file:pw.txt", *extra, "-out", out, cwd=cwd)


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    with tempfile.TemporaryDirectory() as cwd:
        key("root.key", "RSA", "rsa_keygen_bits:2048", cwd)
        self_signed("root.key", ("PODEFA Tests", "PODEFA Signing Test Root"), "root.pem", cwd)
        key("mid.key", "RSA", "rsa_keygen_bits:2048", cwd)
        issued("mid.key", ("PODEFA Tests", "PODEFA Signing Test Intermediate"), "root.pem",
               "root.key", "mid.pem", "ca", cwd)
        key("rsa.key", "RSA", "rsa_keygen_bits:2048", cwd)
        issued("rsa.key", ("PODEFA Tests", "Ayşe Gül Yıldız"), "mid.pem", "mid.key", "rsa.pem",
               "leaf", cwd)
        key("ec.key", "EC", "ec_paramgen_curve:P-256", cwd)
        issued("ec.key", ("PODEFA Tests", "Elif Eğri"), "mid.pem", "mid.key", "ec.pem", "leaf", cwd)
        key("p384.key", "EC", "ec_paramgen_curve:P-384", cwd)
        issued("p384.key", ("PODEFA Tests", "Çağrı Öz"), "mid.pem", "mid.key", "p384.pem", "leaf",
               cwd)

        path = lambda name: os.path.join(here, name)
        p12(path("signer_rsa.p12"), "rsa.key", "rsa.pem", PASSWORD, [], cwd)
        p12(path("signer_legacy.p12"), "rsa.key", "rsa.pem", "eski", ["-legacy"], cwd)
        p12(path("signer_ec.p12"), "ec.key", "ec.pem", PASSWORD, [], cwd)
        p12(path("signer_p384.p12"), "p384.key", "p384.pem", PASSWORD, [], cwd)
        p12(path("signer_empty.p12"), "rsa.key", "rsa.pem", "", [], cwd)
        shutil.copy(os.path.join(cwd, "root.pem"), path("signer_root.pem"))


if __name__ == "__main__":
    main()
