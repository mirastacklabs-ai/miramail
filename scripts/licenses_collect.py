#!/usr/bin/env python3
"""Collect third-party packages (npm bundle + Rust crates) and write
THIRD_PARTY_LICENSES.txt and THIRD_PARTY_NOTICES.md. Offline, deterministic.
Usage: licenses_collect.py <sourcemap-dir> <repo-root>"""
import sys, os, re, json, glob, subprocess, hashlib

maps_dir, root = sys.argv[1], sys.argv[2]
app = os.path.join(root, "app")

# ---------- npm ----------
# Package set = production closure of app/package-lock.json (conservative superset of the bundle: includes
# modules embedded inside pre-bundled files such as roughjs/bundled) minus things that are never bundled.
EXCLUDE = {"elkjs", "@antfu/install-pkg", "package-manager-detector", "tinyexec", "commander", "import-meta-resolve",
           "iconv-lite", "safer-buffer", "rw"}  # elkjs: stubbed; others: Node-only CLI/loader deps
lock = json.load(open(os.path.join(app, "package-lock.json")))["packages"]
pk, seen_npm = [], set()
inmap = set()
if maps_dir != "-":
    for mp in glob.glob(os.path.join(maps_dir, "**", "*.map"), recursive=True):
        for src in json.load(open(mp)).get("sources", []):
            if "node_modules/" in src:
                t = src.rsplit("node_modules/", 1)[1].split("/")
                inmap.add("/".join(t[:2]) if t[0].startswith("@") else t[0])
print("bundle packages (sourcemaps):", len(inmap))
for k, v in sorted(lock.items()):
    if not k: continue
    name = k.split("node_modules/")[-1]
    if v.get("dev") and name not in inmap: continue
    if name in EXCLUDE or name.startswith("@types/"): continue
    d = os.path.join(root, "app", k)
    if (name, v["version"]) in seen_npm or not os.path.isdir(d): continue
    seen_npm.add((name, v["version"]))
    j = json.load(open(os.path.join(d, "package.json")))
    lic = j.get("license") or (j.get("licenses") and "/".join(x.get("type", "?") for x in j["licenses"])) or "UNKNOWN"
    if isinstance(lic, dict): lic = lic.get("type", "UNKNOWN")
    repo = j.get("repository"); repo = repo.get("url") if isinstance(repo, dict) else repo
    pk.append(dict(kind="npm", name=name, version=j["version"], license=lic, source=(repo or j.get("homepage") or "").replace("git+", "").replace("git://", "https://"), dir=d))
missing = inmap - {p['name'] for p in pk}
print('bundled but not in set:', sorted(missing))
if missing: sys.exit(1)

# ---------- crates ----------
md = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--locked", "--offline", "--filter-platform", "aarch64-apple-darwin"], cwd=root))
byid = {p["id"]: p for p in md["packages"]}
nodes = {n["id"]: n for n in md["resolve"]["nodes"]}
rootid = next(p["id"] for p in md["packages"] if p["name"] == "miramail")
seen, stack = set(), [rootid]
while stack:
    i = stack.pop()
    if i in seen: continue
    seen.add(i)
    for dep in nodes[i]["deps"]:
        if any(k["kind"] is None for k in dep["dep_kinds"]):  # normal deps only
            stack.append(dep["pkg"])
for i in seen:
    p = byid[i]
    if p["source"] is None: continue  # repo's own crates
    if any("proc-macro" in tg["kind"] for tg in p["targets"]):
        continue  # proc-macro: compile-time only
    pk.append(dict(kind="crate", name=p["name"], version=p["version"], license=p["license"] or "UNKNOWN", source=p.get("repository") or p.get("homepage") or "", dir=os.path.dirname(p["manifest_path"])))

# fonts
pk.sort(key=lambda x: (x["kind"] != "npm", x["name"].lower(), x["version"]))

# ---------- licence text ----------
PAT = re.compile(r"^(licen[cs]e|copying|unlicense|notice|copyright|patents)([-_.].*)?$", re.I)
def lic_files(d):
    out = []
    ld = os.path.join(d, "LICENSES")
    if os.path.isdir(ld):
        out += ["LICENSES/" + f for f in sorted(os.listdir(ld)) if os.path.isfile(os.path.join(ld, f))]
    for f in sorted(os.listdir(d)):
        if os.path.isfile(os.path.join(d, f)) and PAT.match(f) and not f.lower().endswith((".rs", ".js", ".json", ".ts")):
            out.append(f)
    return out
def read(p): return open(p, encoding="utf-8", errors="replace").read().replace("\r\n", "\n").strip()
def copyright_lines(t):
    return sorted({l.strip() for l in t.split("\n") if re.match(r"^\s*(copyright|\(c\)|©)", l.strip(), re.I) and len(l) < 300})
def norm(t):
    t = "\n".join(l for l in t.split("\n") if l.strip() and not re.match(r"^\s*(copyright|\(c\)|©)", l.strip(), re.I))
    return re.sub(r"\s+", " ", t).strip()

bodies = {}   # hash -> (body, set(pkgs), set(copyright))
notfound, notices = [], {}
for p in pk:
    p["files"] = lic_files(p["dir"]) if os.path.isdir(p["dir"]) else []
    if not p["files"]: notfound.append(p); continue
    for f in p["files"]:
        t = read(os.path.join(p["dir"], f))
        if not t: continue
        if re.search(r"GNU (GENERAL|LESSER|AFFERO)", t[:600], re.I) and re.search(r"\bOR\b", p["license"]):
            continue  # dual-licensed: the permissive option is taken, GPL/LGPL text not shipped
        if p["license"] == "UNKNOWN" and "Permission is hereby granted, free of charge" in t: p["license"] = "MIT (per licence file; package.json has no licence field)"
        if os.path.basename(f).lower().startswith("notice") and "apache license" not in t[:400].lower():
            notices.setdefault(p["name"] + " " + p["version"], t); continue
        h = hashlib.sha256(norm(t).encode()).hexdigest()
        e = bodies.setdefault(h, [t, [], set()])
        e[1].append(p["name"] + " " + p["version"])
        e[2].update(copyright_lines(t))
        if len(t) < len(e[0]) and False: e[0] = t
fonts = []
for fam, fn in (("Oswald", "OFL-Oswald.txt"), ("JetBrains Mono", "OFL-JetBrainsMono.txt")):
    fonts.append((fam, fn, read(os.path.join(root, "scripts", "font-licenses", fn))))
OUT = os.path.join(root, "THIRD_PARTY_LICENSES.txt")
w = []
w.append("MiraMail third-party licence texts\n=================================\nGenerated by scripts/gen-licenses.sh. Where a package is offered under several licences, MiraMail uses the permissive option (MIT, Apache-2.0, BSD, ISC or similar). Packages sharing an identical licence text are grouped; copyright lines are listed as found in each package.\nMPL-2.0 packages are used unmodified; source is available from the repository URLs in THIRD_PARTY_NOTICES.md.\n")
n = 0
for h, (body, names, cr) in sorted(bodies.items(), key=lambda kv: (-len(kv[1][1]), kv[0])):
    n += 1
    w.append("=" * 78 + f"\nGROUP {n}\nPackages ({len(names)}): " + ", ".join(sorted(set(names))) + "\n")
    if cr: w.append("Copyright lines:\n" + "\n".join("  " + c for c in sorted(cr)) + "\n")
    w.append("Licence text:\n\n" + body + "\n")
if notices:
    w.append("=" * 78 + "\nNOTICE files\n")
    for k in sorted(notices): w.append(f"--- {k} ---\n{notices[k]}\n")
w.append("=" * 78 + "\nBundled fonts\n")
for fam, pkgname, t in fonts:
    w.append(f"--- {fam} (scripts/font-licenses/{pkgname}) ---\n" + (t if t else "licence file not found on disk; SIL Open Font License 1.1 per package metadata") + "\n")
w.append("=" * 78 + "\nPackages with no licence file on disk (SPDX id from metadata)\n")
for p in notfound:
    w.append(f"  {p['name']} {p['version']}: {p['license']}  {p['source']}")
open(OUT, "w").write("\n".join(w) + "\n")

# ---------- notices md ----------
src = open(os.path.join(root, "THIRD_PARTY_NOTICES.md")).read()
head = src.split("## ")[0].split("\n\nFull licence")[0].rstrip() + "\n\nFull licence and copyright texts are in THIRD_PARTY_LICENSES.txt (also shown in Settings, About).\n"
md_out = [head]
for kind, title in (("crate", "Rust crates"), ("npm", "npm packages (bundled in the app UI)")):
    md_out.append(f"\n## {title}\n\n| Package | Version | Licence | Source |\n|---|---|---|---|")
    for p in pk:
        if p["kind"] == kind: md_out.append(f"| {p['name']} | {p['version']} | {p['license']} | {p['source']} |")
md_out.append("\n## Notes\n\nelkjs (EPL-2.0) is a mermaid dependency but is deliberately NOT bundled: the build replaces it with a stub (`app/src/lib/elkstub.ts`).\n")
open(os.path.join(root, "THIRD_PARTY_NOTICES.md"), "w").write("\n".join(md_out))

bad = [p for p in pk if re.search(r"GPL|EPL|AGPL", p["license"])]
print(f"npm={sum(p['kind']=='npm' for p in pk)} crates={sum(p['kind']=='crate' for p in pk)} groups={n} notfound={len(notfound)}")
for p in bad: print("FLAG", p["name"], p["version"], p["license"])
