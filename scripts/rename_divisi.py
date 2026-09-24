#!/usr/bin/env python3
"""Staged single -> divisi rename. Explicit tables only; never a blind sed.

Usage: scripts/rename_divisi.py <crates|idents|names|env|paths|prose|check> [--dry-run]
"""
import fnmatch
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Never rewritten: vendored code, build output, lockfile, history, and the docs
# that describe the rename itself.
# Release plumbing (.github, docker, install.sh) is Phase B: it ships as one unit with the first divisi release.
SKIP_PREFIXES = ("vendor/", "target/", ".git/", "docs/adr/", "docs/superpowers/", ".github/", "docker/")
SKIP_FILES = {"Cargo.lock", "CHANGELOG.md", "install.sh", "scripts/rename_divisi.py", "scripts/legacy-allowlist.txt"}
# Files that must keep legacy names on purpose (created after the rename stages).
STAGE_SKIP = {
    "env": {"crates/divisi-core/src/env.rs"},
}

# Persisted identifiers that keep their legacy names in 0.24.0 (spec section 4).
PROTECT = [
    re.compile(p)
    for p in (
        r"naviNBRuas/SingleCLI",
        r"single-redact-master-key",
        r'"single_memory"',
    )
]

CRATE_DIRS = {
    "single-core": "divisi-core",
    "single-protocol": "divisi-protocol",
    "single-runtime": "divisi-runtime",
    "single-agent-sdk": "divisi-agent-sdk",
    "single-native-agent": "divisi-native-agent",
    "single-cli": "divisi-cli",
    "single-tui": "divisi-tui",
    "single-web": "divisi-web",
    "single-lsp": "divisi-lsp",
    "single-notch": "divisi-notch",
    "single-mcp": "divisi-gateway",
    "singlecli-mcp": "divisi-mcp",
}
# Cargo.toml tokens: crate dirs/packages plus the bins and the native-agent package.
CARGO_TOKENS = dict(CRATE_DIRS, **{"single-runtimed": "divisid", "single-agent": "divisi-agent"})

IDENTS = [
    (r"\bsingle_agent_sdk\b", "divisi_agent_sdk"),
    (r"\bsingle_native_agent\b", "divisi_native_agent"),
    (r"\bsingle_core\b", "divisi_core"),
    (r"\bsingle_protocol\b", "divisi_protocol"),
    (r"\bsingle_runtime\b", "divisi_runtime"),
    (r"\bsingle_notch\b", "divisi_notch"),
    (r"\bsingle_web\b", "divisi_web"),
    (r"\bsingle_tui\b", "divisi_tui"),
    (r"\bsingle_lsp\b", "divisi_lsp"),
    (r"\bsingle_mcp\b", "divisi_gateway"),
    (r"\bsinglecli_mcp\b", "divisi_mcp"),
    # Cargo's env!("CARGO_BIN_EXE_<bin>"): the `_` before the bin name defeats the token boundary.
    (r"CARGO_BIN_EXE_single-runtimed\b", "CARGO_BIN_EXE_divisid"),
    (r"CARGO_BIN_EXE_singlecli-mcp\b", "CARGO_BIN_EXE_divisi-mcp"),
    (r"CARGO_BIN_EXE_single-mcp\b", "CARGO_BIN_EXE_divisi-gateway"),
    (r"CARGO_BIN_EXE_single-(lsp|notch)\b", r"CARGO_BIN_EXE_divisi-\1"),
    (r"CARGO_BIN_EXE_single\b(?!-)", "CARGO_BIN_EXE_divisi"),
    (r"\bSingleDirs\b", "DivisiDirs"),
    (r"\bSingleCliServer\b", "DivisiServer"),
    (r"\bSingleAgentAdapter\b", "DivisiAgentAdapter"),
]

NAME_PAIRS = [
    ("single-runtimed", "divisid"),
    ("singlecli-mcp", "divisi-mcp"),
    ("single-mcp", "divisi-gateway"),
    ("single-lsp", "divisi-lsp"),
    ("single-notch", "divisi-notch"),
    ("single-cli", "divisi-cli"),
    ("single-tui", "divisi-tui"),
    ("single-web", "divisi-web"),
    ("single-runtime", "divisi-runtime"),
    ("single-protocol", "divisi-protocol"),
    ("single-core", "divisi-core"),
    ("single-agent-sdk", "divisi-agent-sdk"),
    ("single-native-agent", "divisi-native-agent"),
    ("single.db", "divisi.db"),
]


def token(old):
    return re.compile(r"(?<![\w.-])" + re.escape(old) + r"(?![\w-])")


PATHS = [
    (re.compile(r"\.config/single(?![\w-])"), ".config/divisi"),
    (re.compile(r'join\("single"\)'), 'join("divisi")'),
]

PROSE = [
    (re.compile(r"SingleCLI"), "divisi"),
    (re.compile(r"(?<=`)single(?=[` ])"), "divisi"),
    (re.compile(r"(?<=\$ )single(?= )"), "divisi"),
    (re.compile(r'(?<=")single(?=",)'), "divisi"),
    (re.compile(r'Command::new\("single"\)'), 'Command::new("divisi")'),
    (re.compile(r'name = "single"'), 'name = "divisi"'),
]

LEGACY = re.compile(
    r"SingleCLI|singlecli"
    r"|(?<![\w.-])single-(?:core|protocol|runtimed|runtime|agent-sdk|native-agent|cli|tui|web|lsp|notch|mcp|pool|agent|acp)(?![\w-])"
    r"|(?<![\w.-])single-(?:openrouter|nvidia|google|gemini|typhoon|huggingface|ollama-cloud|mistral|cerebras|cloudflare|opencode-zen)(?![\w-])"
    r"|single/task-"
    r"|\bSINGLE_[A-Z]|\.config/single(?![\w-])"
    r"|\bSingle(?:Dirs|CliServer|AgentAdapter)\b"
    r"|\bsingle_(?:core|protocol|runtime|agent_sdk|native_agent|notch|web|tui|lsp|mcp)\b"
)


def tracked():
    out = subprocess.run(["git", "ls-files"], cwd=ROOT, capture_output=True, text=True, check=True).stdout.split("\n")
    return [p for p in out if p and not p.startswith(SKIP_PREFIXES) and p not in SKIP_FILES]


def read(rel):
    try:
        return (ROOT / rel).read_text(encoding="utf-8")
    except (UnicodeDecodeError, FileNotFoundError, IsADirectoryError):
        return None


def protect(text):
    saved = []

    def stash(m):
        saved.append(m.group(0))
        return f"\x00{len(saved) - 1}\x00"

    for pat in PROTECT:
        text = pat.sub(stash, text)
    return text, saved


def restore(text, saved):
    return re.sub(r"\x00(\d+)\x00", lambda m: saved[int(m.group(1))], text)


def rewrite(files, transform, dry):
    changed = 0
    for rel in files:
        text = read(rel)
        if text is None:
            continue
        new = transform(rel, text)
        if new != text:
            changed += 1
            print(("would change " if dry else "changed ") + rel)
            if not dry:
                (ROOT / rel).write_text(new, encoding="utf-8")
    print(f"{changed} file(s) {'would change' if dry else 'changed'}")


def git_mv(src, dst, dry):
    if not (ROOT / src).exists():
        return
    print(f"mv {src} -> {dst}")
    if not dry:
        subprocess.run(["git", "mv", src, dst], cwd=ROOT, check=True)


def stage_crates(dry):
    for old, new in CRATE_DIRS.items():
        git_mv(f"crates/{old}", f"crates/{new}", dry)
    git_mv("crates/divisi-runtime/src/bin/single-runtimed.rs", "crates/divisi-runtime/src/bin/divisid.rs", dry)
    git_mv("extensions/gnome-shell/single-notch@nbr.company", "extensions/gnome-shell/divisi-notch@nbr.company", dry)
    tokens = [(token(o), n) for o, n in CARGO_TOKENS.items()]

    def cargo(rel, text):
        for pat, new in tokens:
            text = pat.sub(new, text)
        return text

    rewrite([p for p in tracked() if p.endswith("Cargo.toml")], cargo, dry)


def stage_idents(dry):
    pats = [(re.compile(p), n) for p, n in IDENTS]

    def f(rel, text):
        for pat, new in pats:
            text = pat.sub(new, text)
        return text

    rewrite([p for p in tracked() if p.endswith((".rs", ".md", ".toml"))], f, dry)


def regex_stage(pairs, dry, skip=()):
    def f(rel, text):
        text, saved = protect(text)
        for pat, new in pairs:
            text = pat.sub(new, text)
        return restore(text, saved)

    rewrite([p for p in tracked() if p not in skip], f, dry)


def stage_check():
    allow = []
    allow_file = ROOT / "scripts/legacy-allowlist.txt"
    if allow_file.exists():
        allow = [l.split("\t")[0].strip() for l in allow_file.read_text().splitlines() if l.strip() and not l.startswith("#")]
    bad = 0
    listing = subprocess.run(["git", "ls-files"], cwd=ROOT, capture_output=True, text=True, check=True).stdout.split("\n")
    for rel in listing:
        if not rel or rel.startswith(("vendor/", "target/")) or rel in SKIP_FILES or rel == "Cargo.lock":
            continue
        if rel.startswith("docs/superpowers/") or rel.startswith("docs/adr/"):
            continue
        if any(fnmatch.fnmatch(rel, g) for g in allow):
            continue
        text = read(rel)
        if text is None:
            continue
        for n, line in enumerate(text.splitlines(), 1):
            stripped, _ = protect(line)
            if LEGACY.search(stripped):
                bad += 1
                print(f"{rel}:{n}: {line.strip()[:140]}")
    print(f"{bad} legacy reference(s) outside the allowlist")
    return 1 if bad else 0


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    dry = "--dry-run" in sys.argv
    if len(args) != 1:
        sys.exit(__doc__)
    stage = args[0]
    if stage == "crates":
        stage_crates(dry)
    elif stage == "idents":
        stage_idents(dry)
    elif stage == "names":
        regex_stage([(token(o), n) for o, n in NAME_PAIRS], dry)
    elif stage == "env":
        regex_stage([(re.compile(r"\bSINGLE_(?=[A-Z])"), "DIVISI_")], dry, skip=STAGE_SKIP["env"])
    elif stage == "paths":
        regex_stage(PATHS, dry)
    elif stage == "prose":
        regex_stage(PROSE, dry)
    elif stage == "check":
        sys.exit(stage_check())
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
