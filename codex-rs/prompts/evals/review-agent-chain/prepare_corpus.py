import argparse
import json
import pathlib
import subprocess


def run(*args, cwd):
    subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True)
    args = parser.parse_args()
    here = pathlib.Path(__file__).resolve().parent
    corpus = json.loads((here / "corpus.json").read_text(encoding="utf-8"))
    root = pathlib.Path(args.root).resolve()
    root.mkdir(parents=True, exist_ok=True)
    cases_root = root / "cases"
    cases_root.mkdir(exist_ok=True)

    for name, case in corpus.items():
        case_root = cases_root / name
        case_root.mkdir(exist_ok=True)
        run("git", "init", "--initial-branch=main", cwd=case_root)
        run("git", "config", "user.name", "Review Eval", cwd=case_root)
        run("git", "config", "user.email", "review-eval@example.com", cwd=case_root)
        for relative, versions in case["files"].items():
            path = case_root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(versions["before"], encoding="utf-8")
        run("git", "add", ".", cwd=case_root)
        run("git", "commit", "-m", "baseline", cwd=case_root)
        for relative, versions in case["files"].items():
            (case_root / relative).write_text(versions["after"], encoding="utf-8")

    (root / "oracles.json").write_text(
        json.dumps(
            {
                name: {key: value for key, value in case.items() if key != "files"}
                for name, case in corpus.items()
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()
