import argparse
import collections
import hashlib
import json
import re
import shutil
import statistics
import subprocess
import tarfile
import tempfile
import time
import urllib.request
from pathlib import Path, PurePosixPath

MAX_ARCHIVE_BYTES = 1024 * 1024 * 1024


def snapshot(entry, cache, fetch):
    repository, revision = entry["repository"], entry["revision"]
    if (
        not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9-]*/[\w.-]+", repository)
        or repository.split("/")[-1] in {".", ".."}
        or not re.fullmatch(r"[0-9a-f]{40}", revision)
    ):
        raise ValueError("invalid repository or pinned revision")
    name = repository.replace("/", "--")
    destination = cache / "repos" / name
    metadata = cache / "metadata" / f"{name}.json"
    if destination.is_dir() and metadata.is_file():
        data = json.loads(metadata.read_text())
        if data.get("revision", data.get("sha")) == revision:
            return destination
    if not fetch:
        raise ValueError(
            f"missing pinned snapshot: {repository}; pass --fetch to download"
        )
    if destination.exists():
        raise ValueError(
            f"unverified snapshot exists: {destination}; use a fresh cache"
        )
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="fetch-", dir=cache) as temporary:
        temporary = Path(temporary)
        archive = temporary / "snapshot.tar.gz"
        with (
            urllib.request.urlopen(
                f"https://codeload.github.com/{repository}/tar.gz/{revision}",
                timeout=90,
            ) as response,
            archive.open("wb") as output,
        ):
            size = 0
            while chunk := response.read(1024 * 1024):
                size += len(chunk)
                if size > MAX_ARCHIVE_BYTES:
                    raise ValueError("archive exceeds 1 GiB")
                output.write(chunk)
        extracted = temporary / "snapshot"
        extracted.mkdir()
        size = 0
        with tarfile.open(archive, "r:gz") as source:
            for member in source:
                parts = PurePosixPath(member.name).parts
                if not parts or member.name.startswith("/") or ".." in parts:
                    raise ValueError("unsafe archive path")
                if len(parts) == 1:
                    continue
                path = extracted.joinpath(*parts[1:])
                if member.isdir():
                    path.mkdir(parents=True, exist_ok=True)
                elif member.isfile():
                    size += member.size
                    if size > 2 * MAX_ARCHIVE_BYTES:
                        raise ValueError("expanded snapshot exceeds 2 GiB")
                    path.parent.mkdir(parents=True, exist_ok=True)
                    with (
                        source.extractfile(member) as content,
                        path.open("wb") as output,
                    ):
                        shutil.copyfileobj(content, output)
        extracted.rename(destination)
    metadata.parent.mkdir(parents=True, exist_ok=True)
    metadata.write_text(
        json.dumps({"repository": repository, "revision": revision}) + "\n"
    )
    return destination


def check(projects, expected):
    project = projects.get(expected["path"])
    if project is None:
        return False, "project missing"
    kind = expected["kind"]
    ids = [d["id"] for d in project["detections"]]
    if kind == "language":
        actual = [language["name"] for language in project["languages"]]
        return expected["language"] in actual, actual
    if kind == "detection":
        found = next(
            (d for d in project["detections"] if d["id"] == expected["id"]), None
        )
        roles = expected.get("roles", [])
        return found is not None and set(roles).issubset(
            found.get("roles", [])
        ), project["detections"] if roles else ids
    if kind == "name":
        return expected["name"] in project["names"], project["names"]
    if kind == "workspace":
        actual = project["workspaceMembers"]
        return expected["id"] in ids and expected["member"] in actual, {
            "ids": ids,
            "members": actual,
        }
    if kind == "version":
        actual = [
            v
            for language in project["languages"]
            if language["name"] == expected["language"]
            for v in language["versions"]
        ]
        return any(
            v["value"] == expected["value"]
            and v["source"] == expected["source"]
            and (
                "versionKind" not in expected
                or v.get("kind") == expected["versionKind"]
            )
            for v in actual
        ), actual
    raise ValueError(f"unknown check kind: {kind}")


def audit(entry, args):
    path = snapshot(entry, args.cache, args.fetch)
    samples, first = [], None
    deterministic = True
    for _ in range(args.runs):
        start = time.perf_counter()
        process = subprocess.run(
            [str(args.binary), "detect", str(path), "--json"],
            capture_output=True,
            timeout=90,
            check=False,
        )
        samples.append((time.perf_counter() - start) * 1000)
        if process.returncode:
            raise ValueError(process.stderr.decode(errors="replace"))
        if first is None:
            first = process.stdout
        else:
            deterministic = deterministic and first == process.stdout
    report = json.loads(first)
    if report["schemaVersion"] != 1:
        raise ValueError("unexpected detector schema version")
    projects = {project["path"]: project for project in report["projects"]}
    checks = []
    for expected in entry["checks"]:
        passed, actual = check(projects, expected)
        checks.append({"expected": expected, "passed": passed, "actual": actual})
    return {
        "repository": entry["repository"],
        "language": entry["language"],
        "revision": entry["revision"],
        "firstMs": samples[0],
        "medianMs": statistics.median(samples[1:]),
        "samplesMs": samples,
        "deterministic": deterministic,
        "stats": report["stats"],
        "projectCount": len(projects),
        "diagnostics": report["diagnostics"],
        "checks": checks,
    }


def main():
    parser = argparse.ArgumentParser(
        description="Audit local project detection against pinned public GitHub snapshots without installing or executing project code."
    )
    parser.add_argument(
        "--corpus", type=Path, default=Path(__file__).with_name("detect-corpus.json")
    )
    parser.add_argument(
        "--binary",
        type=Path,
        default=Path(__file__).resolve().parents[1] / "target/release/brainpod",
    )
    parser.add_argument(
        "--cache", type=Path, help="Snapshot cache; required unless --list"
    )
    parser.add_argument(
        "--fetch",
        action="store_true",
        help="Explicitly allow downloading missing pinned GitHub snapshots",
    )
    parser.add_argument(
        "--runs",
        type=int,
        default=5,
        help="CLI invocations per repository; the first is excluded from the warm median",
    )
    parser.add_argument(
        "--output", type=Path, help="Results JSON; required unless --list"
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="List pinned repositories without downloading or running the detector",
    )
    args = parser.parse_args()
    corpus = json.loads(args.corpus.read_text())
    if corpus["schemaVersion"] != 1:
        parser.error("unsupported corpus schema version")
    if args.list:
        for entry in corpus["repositories"]:
            print(f"{entry['language']}\t{entry['repository']}\t{entry['revision']}")
        return 0
    if args.cache is None or args.output is None:
        parser.error("--cache and --output are required unless --list")
    if args.runs < 2:
        parser.error("--runs must be at least 2")
    args.binary = args.binary.resolve(strict=True)
    args.cache = args.cache.resolve()
    args.cache.mkdir(parents=True, exist_ok=True)
    with args.binary.open("rb") as binary:
        binary_sha = hashlib.file_digest(binary, "sha256").hexdigest()
    results = []
    for entry in corpus["repositories"]:
        try:
            result = audit(entry, args)
            passed = sum(c["passed"] for c in result["checks"])
            print(
                f"{entry['repository']}: {passed}/{len(result['checks'])} checks; {result['medianMs']:.2f} ms",
                flush=True,
            )
        except (
            OSError,
            ValueError,
            KeyError,
            tarfile.TarError,
            subprocess.TimeoutExpired,
        ) as error:
            result = {
                "repository": entry["repository"],
                "revision": entry["revision"],
                "error": str(error),
            }
            print(f"{entry['repository']}: ERROR {error}", flush=True)
        results.append(result)
    counts = collections.defaultdict(lambda: {"passed": 0, "total": 0})
    for result in results:
        for item in result.get("checks", []):
            category = counts[item["expected"]["category"]]
            category["total"] += 1
            category["passed"] += item["passed"]
    output = {
        "schemaVersion": 1,
        "selectedAt": corpus["selectedAt"],
        "binarySha256": binary_sha,
        "categories": dict(counts),
        "results": results,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2) + "\n")
    for category, count in sorted(counts.items()):
        print(f"{category}: {count['passed']}/{count['total']}")
    print(f"Results: {args.output}")
    return int(
        any(
            "error" in r
            or not r["deterministic"]
            or any(not c["passed"] for c in r["checks"])
            for r in results
        )
    )


if __name__ == "__main__":
    raise SystemExit(main())
