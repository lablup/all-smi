#!/usr/bin/env python3
"""Print the `run:` body of a named step from a GitHub Actions workflow file.

The whole point of the tests around update_homebrew_formula.yml is that they
execute the committed YAML rather than a copy of it that drifts. Extracting the
step body by name keeps the thing under test and the thing that ships identical,
so an edit to the workflow that breaks a documented behaviour fails a test even
when nobody thought to update the test alongside it.

Usage: extract-step.py WORKFLOW.yml 'Step name'
"""

import sys

import yaml


def find_step(workflow: dict, name: str):
    for job in (workflow.get("jobs") or {}).values():
        for step in job.get("steps") or []:
            if step.get("name") == name:
                return step
    return None


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        sys.stderr.write("usage: extract-step.py WORKFLOW.yml 'Step name'\n")
        return 2

    path, name = argv[1], argv[2]
    with open(path, encoding="utf-8") as handle:
        workflow = yaml.safe_load(handle)

    step = find_step(workflow, name)
    if step is None:
        sys.stderr.write(f"step {name!r} not found in {path}\n")
        return 1

    body = step.get("run")
    if body is None:
        sys.stderr.write(f"step {name!r} has no run: body\n")
        return 1

    sys.stdout.write(body)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
