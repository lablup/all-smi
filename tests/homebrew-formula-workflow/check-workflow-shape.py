#!/usr/bin/env python3
"""Structural checks on update_homebrew_formula.yml that no step body can make.

Three things this file has to keep true are properties of the YAML rather than
of any script inside it: nothing interpolates a workflow expression into a
shell, the job asks for no more permission than it uses, and the tap secret is
named in exactly one place. Each prints one `ok`/`FAIL` line, in the same shape
the shell cases use.

Usage: check-workflow-shape.py WORKFLOW.yml
"""

import sys

import yaml

TAP_SECRET = "HOMEBREW_TAP_TOKEN"


def steps_of(workflow: dict):
    for job_name, job in (workflow.get("jobs") or {}).items():
        for step in job.get("steps") or []:
            yield job_name, step


def report(results: list[tuple[bool, str, str]]) -> int:
    failures = 0
    for good, label, detail in results:
        if good:
            print(f"  ok    {label}")
        else:
            failures += 1
            print(f"  FAIL  {label}")
            if detail:
                print(f"        {detail}")
    return failures


def check(workflow: dict, raw: str) -> list[tuple[bool, str, str]]:
    results = []

    # A workflow expression in a `run:` body is substituted textually before
    # bash parses the line, so a value carrying shell metacharacters runs as
    # code. Every value this workflow needs is passed through `env:` instead.
    offenders = [
        f"{job}/{step.get('name')}"
        for job, step in steps_of(workflow)
        if step.get("run") and "${{" in step["run"]
    ]
    results.append(
        (
            not offenders,
            "no step body interpolates a workflow expression",
            "offending steps: " + ", ".join(offenders),
        )
    )

    permissions = workflow.get("permissions")
    results.append(
        (
            permissions == {"contents": "read"},
            "workflow permissions are contents: read and nothing else",
            f"found: {permissions!r}",
        )
    )

    concurrency = workflow.get("concurrency") or {}
    results.append(
        (
            bool(concurrency.get("group")),
            "a concurrency group serializes formula updates",
            f"found: {concurrency!r}",
        )
    )

    # The tap token must reach exactly one step, through that step's `env:`,
    # and it must be read there and nowhere else. One `secrets.` reference in
    # the whole file is that shape; a second one is a second exposure.
    references = raw.count(f"secrets.{TAP_SECRET}")
    carriers = [
        f"{job}/{step.get('name')}"
        for job, step in steps_of(workflow)
        if TAP_SECRET in (step.get("env") or {})
    ]
    results.append(
        (
            references == 1 and len(carriers) == 1,
            f"{TAP_SECRET} is read once, into one step's env:",
            f"{references} secrets.{TAP_SECRET} references, carried by: {carriers}",
        )
    )

    # Comments in this workflow describe the credential-in-url shape at length
    # in order to explain why it is not used, so the check is against the code
    # the shell actually runs. `username=x-access-token` in the credential
    # helper is the intended form and carries no colon after the name.
    code = "\n".join(
        line
        for _, step in steps_of(workflow)
        for line in (step.get("run") or "").splitlines()
        if not line.lstrip().startswith("#")
    )
    results.append(
        (
            "x-access-token:" not in code,
            "no credential is embedded in a git url",
            "a shell line builds an x-access-token: url",
        )
    )

    return results


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        sys.stderr.write("usage: check-workflow-shape.py WORKFLOW.yml\n")
        return 2
    with open(argv[1], encoding="utf-8") as handle:
        raw = handle.read()
    workflow = yaml.safe_load(raw)
    return 1 if report(check(workflow, raw)) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
