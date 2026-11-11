#!/usr/bin/env python3
"""Build a webOS .ipk (ar archive: debian-binary, control.tar.gz, data.tar.gz) without ares-package.

usage: mkipk.py --binary tidal-webos --lib libSDL2-2.0.so.0 [--lib ...] --out out.ipk
Layout (learned from existing native webOS packages):
  usr/palm/applications/<id>/{appinfo.json, <main>, icon.png, largeIcon.png, lib/*}
  usr/palm/packages/<id>/packageinfo.json
Unverified on a TV until you install it: check `ares-install` / Dev Manager output.
"""
import argparse, gzip, io, json, struct, sys, tarfile, zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
EPOCH = 0


def png(size, bg=(11, 13, 18), fg=(46, 230, 208)):
    """Placeholder icon: dark square with an accent disc (pure stdlib PNG)."""
    cx = cy = (size - 1) / 2
    r = size * 0.30
    rows = []
    for y in range(size):
        row = bytearray([0])
        for x in range(size):
            d = ((x - cx) ** 2 + (y - cy) ** 2) ** 0.5
            a = max(0.0, min(1.0, r - d + 0.5))
            row += bytes(int(bg[i] + (fg[i] - bg[i]) * a) for i in range(3))
        rows.append(bytes(row))
    raw = b"".join(rows)

    def chunk(t, d):
        c = struct.pack(">I", len(d)) + t + d
        return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def targz(entries):
    """entries: list of (name, bytes, mode). Deterministic ownership/mtime."""
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.GNU_FORMAT) as tf:
        dirs = set()
        for name, _, _ in entries:
            parts = name.split("/")
            for i in range(2, len(parts)):  # skip "." and top-level
                dirs.add("/".join(parts[:i]))
        for d in sorted(dirs | {"."}):
            ti = tarfile.TarInfo(d)
            ti.type, ti.mode, ti.mtime, ti.uid, ti.gid = tarfile.DIRTYPE, 0o755, EPOCH, 0, 0
            tf.addfile(ti)
        for name, blob, mode in entries:
            ti = tarfile.TarInfo(name)
            ti.size, ti.mode, ti.mtime, ti.uid, ti.gid = len(blob), mode, EPOCH, 0, 0
            tf.addfile(ti, io.BytesIO(blob))
    out = io.BytesIO()
    with gzip.GzipFile(filename="", mode="wb", fileobj=out, mtime=0, compresslevel=9) as gz:
        gz.write(raw.getvalue())
    return out.getvalue()


def write_ar(path, members):
    with open(path, "wb") as fh:
        fh.write(b"!<arch>\n")
        for name, blob in members:
            fh.write(f"{name:<16}{EPOCH:<12}{0:<6}{0:<6}{'100644':<8}{len(blob):<10}".encode() + b"\x60\n")
            fh.write(blob)
            if len(blob) % 2:
                fh.write(b"\n")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", required=True)
    ap.add_argument("--lib", action="append", default=[])
    ap.add_argument("--out", required=True)
    ap.add_argument("--appinfo", default=str(HERE / "appinfo.json"))
    ap.add_argument("--icon",       default=None, help="icon.png (80x80)")
    ap.add_argument("--large-icon", default=None, help="largeIcon.png (130x130)")
    a = ap.parse_args()

    app = json.loads(Path(a.appinfo).read_text())
    app_id, main_name = app["id"], app["main"]
    icon_blob      = Path(a.icon).read_bytes()       if a.icon       else png(80)
    large_icon_blob = Path(a.large_icon).read_bytes() if a.large_icon else png(130)
    base = f"./usr/palm/applications/{app_id}"
    data = [
        (f"{base}/appinfo.json", json.dumps(app, indent=2).encode() + b"\n", 0o644),
        (f"{base}/{main_name}", Path(a.binary).read_bytes(), 0o755),
        (f"{base}/icon.png", icon_blob, 0o644),
        (f"{base}/largeIcon.png", large_icon_blob, 0o644),
    ]
    for lib in a.lib:
        p = Path(lib)
        data.append((f"{base}/lib/{p.name}", p.read_bytes(), 0o755))
    pkginfo = {"app": app_id, "id": app_id, "loc_name": app["title"], "package_format_version": 2,
               "vendor": app["vendor"], "version": app["version"]}
    data.append((f"./usr/palm/packages/{app_id}/packageinfo.json", json.dumps(pkginfo, indent=4).encode() + b"\n", 0o644))

    kib = (sum(len(b) for _, b, _ in data) + 1023) // 1024
    control = "\n".join([
        f"Package: {app_id}", f"Version: {app['version']}", "Section: misc", "Priority: optional",
        "Architecture: arm", f"Installed-Size: {kib}", f"Maintainer: {app['vendor']} <noreply@example.invalid>",
        f"Description: {app.get('appDescription', app['title'])}",
        "webOS-Package-Format-Version: 2", "webOS-Packager-Version: tide-mkipk/1", ""]).encode()
    write_ar(a.out, [("debian-binary", b"2.0\n"),
                     ("control.tar.gz", targz([("./control", control, 0o644)])),
                     ("data.tar.gz", targz(data))])
    print(f"wrote {a.out} ({kib} KiB installed)")


if __name__ == "__main__":
    sys.exit(main())
