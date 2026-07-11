#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Calibrate deterministic Aden context tiers against the agent task corpus."""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import subprocess
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("agent_bench", ROOT / "scripts/agent_bench.py")
assert SPEC and SPEC.loader
agent_bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(agent_bench)

BUDGETS = (512, 1024, 2048, 4096)
RISK_WORDS = re.compile(
    r"\b(security|permission|auth|migration|transaction|concurren|lock|corrupt|public api|impact|break)\w*\b",
    re.IGNORECASE,
)
DEEP_WORDS = re.compile(r"\b(debug|root cause|what breaks|blast radius|compare|architecture)\b", re.IGNORECASE)
EXPLICIT_TARGET = re.compile(r"`[^`]+`|\b[A-Za-z_][A-Za-z0-9_]*(?:::|\.)[A-Za-z_][A-Za-z0-9_]*\b")
SNAKE_TARGET = re.compile(r"\b[A-Za-z][A-Za-z0-9]*_[A-Za-z0-9_]+\b")
STOP_WORDS = {"what", "when", "where", "which", "does", "with", "from", "into", "during", "this", "that", "required", "agent"}
QUERY_EXPANSIONS = {
    "availability": {"available", "unavailable", "closed"},
    "configuration": {"config", "bridge"},
    "failure": {"error"},
    "lifecycle": {"bridge", "config", "open", "close", "state", "webview"},
    "processing": {"pipeline"},
    "session": {"browser"},
    "types": {"error"},
}
# Carry only the stable product-domain root into decomposed facets. Carrying every
# noun from the full question contaminates otherwise independent evidence roles.
DOMAIN_TERMS = {"preview"}


@dataclass(frozen=True)
class ContextPlan:
    route: str
    tier: str
    budget: int
    ceiling: int
    required_roles: tuple[str, ...]
    reasons: tuple[str, ...]


ROLE_MAP = {
    "contract_lookup": ("contract",),
    "symbol_explanation": ("definition", "implementation"),
    "concept_discovery": ("overview", "implementation"),
    "dependency_trace": ("target", "relationship", "behavior"),
    "architecture": ("entry_point", "components", "contract"),
}


def context_plan(question: str, category: str) -> ContextPlan:
    roles = ROLE_MAP.get(category, ("implementation",))
    reasons: list[str] = [f"category={category}"]
    if category == "contract_lookup":
        route, tier, ceiling = "text", "direct", 512
    elif category in {"symbol_explanation", "concept_discovery"}:
        route, tier, ceiling = "aden", "compact", 1024
    else:
        route, tier, ceiling = "aden", "connected", 2048
    if EXPLICIT_TARGET.search(question) and category == "symbol_explanation":
        reasons.append("explicit_target")
    if RISK_WORDS.search(question) and ceiling < 2048:
        tier, ceiling = "connected", 2048
        reasons.append("risk_floor")
    if DEEP_WORDS.search(question):
        route, tier, ceiling = "aden", "deep", 4096
        reasons.append("deep_intent")
    return ContextPlan(route, tier, 512, ceiling, roles, tuple(reasons))


def context_has_pattern(context: str, patterns: list[str]) -> bool:
    # Strip presentation delimiters, but preserve underscores inside code identifiers.
    normalized = re.sub(r"[*`]+", "", context)
    normalized = re.sub(r"\s+", " ", normalized)
    return any(re.search(pattern, normalized, re.IGNORECASE) for pattern in patterns)


def score_context(task: dict[str, Any], context: str) -> dict[str, Any]:
    facts = {
        fact["id"]: context_has_pattern(context, fact["any_of"])
        for fact in task["required_facts"]
    }
    evidence = {
        expected.get("id", str(index)): context_has_pattern(context, expected["any_of"])
        for index, expected in enumerate(task.get("expected_evidence", []), start=1)
    }
    fact_recall = sum(facts.values()) / len(facts)
    evidence_recall = sum(evidence.values()) / len(evidence) if evidence else 1.0
    return {
        "fact_recall": round(fact_recall, 4),
        "evidence_recall": round(evidence_recall, 4),
        "sufficient": fact_recall == 1.0 and evidence_recall == 1.0,
        "facts": facts,
        "evidence": evidence,
    }


def query_terms(text: str) -> set[str]:
    return {word for word in re.findall(r"[a-z0-9]+", text.lower()) if len(word) >= 3 and word not in STOP_WORDS}


def intent_queries(question: str) -> list[str]:
    """Produce deterministic facet queries without asking a model to route."""
    clean = question.strip().rstrip("?.!")
    facets = [part.strip() for part in re.split(r",|\band\b", clean, flags=re.IGNORECASE)]
    facets = [part for part in facets if len(query_terms(part)) >= 2]
    shared = query_terms(question) & DOMAIN_TERMS
    queries = [question]
    for facet in [clean, *facets]:
        terms = query_terms(facet) | shared
        expanded = terms | {word for term in terms for word in QUERY_EXPANSIONS.get(term, set())}
        queries.append(" ".join(sorted(expanded)))
    unique: list[str] = []
    seen: set[str] = set()
    for query in queries:
        for candidate in (query,):
            normalized = candidate.casefold().strip()
            if normalized and normalized not in seen:
                unique.append(candidate)
                seen.add(normalized)
    return unique


def explicit_symbol(question: str) -> str | None:
    quoted = re.findall(r"`([A-Za-z_][A-Za-z0-9_:.-]*)`", question)
    if quoted:
        return quoted[0].removesuffix("()")
    match = SNAKE_TARGET.search(question)
    return match.group(0) if match else None


def routing_policy(task: dict[str, Any]) -> str:
    """Choose a retrieval primitive from observable syntax and task shape."""
    category = task["category"]
    question = task["question"].lstrip().casefold()
    if category == "dependency_trace":
        return "adaptive" if explicit_symbol(task["question"]) else "ask"
    if category == "contract_lookup":
        return "adaptive"
    if category == "symbol_explanation":
        return "adaptive" if explicit_symbol(task["question"]) else "ask"
    if category == "concept_discovery":
        return "ask" if question.startswith("where ") else "adaptive"
    if category == "architecture":
        return "ask" if question.startswith("how ") else "adaptive"
    return "ask"


def run_json(command: list[str], timeout: int) -> dict[str, Any]:
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    if result.returncode != 0:
        raise RuntimeError(result.stderr[-500:] or f"command exited {result.returncode}")
    return json.loads(result.stdout)


def anchor_file(anchor: str) -> str:
    return anchor.split("#", 1)[0]


def anchor_role(anchor: str) -> str:
    path = anchor_file(anchor).removeprefix("aden://module/")
    return "/".join(path.split("/")[:3])


def adaptive_anchors(aden_bin: str, repo: Path, task: dict[str, Any], timeout: int) -> list[str]:
    symbol = explicit_symbol(task["question"])
    if symbol and task["category"] == "symbol_explanation":
        payload = run_json([aden_bin, "locate", "--project", str(repo), "--symbol", symbol, "--json"], timeout)
        items = payload.get("items", [])
        return [items[0].get("anchor")] if items else []
    if symbol and task["category"] == "dependency_trace":
        payload = run_json(
            [aden_bin, "grep", "--project", str(repo), "--json", symbol], timeout
        )
        callers = [
            match.get("anchor") for match in payload.get("matches", [])
            if match.get("anchor")
            and not match.get("anchor", "").endswith(f"#{symbol}")
            and "#tests" not in match.get("anchor", "")
            and "/test" not in match.get("file", "").lower()
            and not match.get("file", "").startswith("scripts/")
        ]
        if callers:
            return callers[:1]
    terms = query_terms(task["question"])
    queries = intent_queries(task["question"])
    results_by_anchor: dict[str, dict[str, Any]] = {}
    query_rankings: list[list[str]] = []
    def search(query: str) -> dict[str, Any]:
        return run_json(
            [aden_bin, "search", "--project", str(repo), "--limit", "12", "--json", query], timeout
        )
    with ThreadPoolExecutor(max_workers=min(6, len(queries))) as pool:
        payloads = list(pool.map(search, queries))
    for payload in payloads:
        ranking: list[str] = []
        for result in payload.get("results", []):
            anchor = result.get("anchor", "")
            if anchor:
                ranking.append(anchor)
                results_by_anchor.setdefault(anchor, result)
        query_rankings.append(ranking)
    results = sorted(
        results_by_anchor.values(), key=lambda result: float(result.get("score", 0.0)), reverse=True
    )[:16]
    if not results:
        return []
    cross_boundary = "lifecycle" in terms or {"desktop", "web"} <= terms
    desired = 3 if task["category"] == "architecture" or cross_boundary else (2 if task["category"] == "concept_discovery" else 1)
    if desired > 1:
        selected: list[str] = []
        files: set[str] = set()
        roles: set[str] = set()
        for ranking in query_rankings[2:]:
            for anchor in ranking:
                source = anchor_file(anchor)
                if source not in files:
                    selected.append(anchor)
                    files.add(source)
                    roles.add(anchor_role(anchor))
                    break
            if len(selected) >= desired:
                return selected
        # Facet rank is already evidence-role specific. Fill any remaining slot by
        # source-layer diversity without paying for redundant assembly probes.
        for require_new_role in (True, False):
            for result in results:
                anchor = result.get("anchor", "")
                source = anchor_file(anchor)
                role = anchor_role(anchor)
                if not anchor or source in files or (require_new_role and role in roles):
                    continue
                selected.append(anchor)
                files.add(source)
                roles.add(role)
                if len(selected) >= desired:
                    return selected
        return selected
    def probe(result: dict[str, Any]) -> tuple[dict[str, Any], str]:
        anchor = result.get("anchor", "")
        command = [aden_bin, "asm", "--human", "--silent", "--strict", "--budget", "128", "--project", str(repo), "--from", anchor]
        completed = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
        return result, completed.stdout if completed.returncode == 0 else ""
    with ThreadPoolExecutor(max_workers=min(8, len(results))) as pool:
        probes = list(pool.map(probe, results))
    top_score = max(float(result.get("score", 0.0)) for result in results) or 1.0
    def key(item: tuple[dict[str, Any], str]) -> tuple[float, float]:
        result, body = item
        coverage = len(terms & query_terms(body)) / max(1, len(terms))
        return coverage, float(result.get("score", 0.0)) / top_score
    ranked = sorted(probes, key=key, reverse=True)
    desired = 1
    selected: list[str] = []
    files: set[str] = set()
    roles: set[str] = set()
    ranked_anchors = [result.get("anchor", "") for result, _ in ranked]
    # Cover distinct question facets first; only then fill by global relevance.
    for ranking in query_rankings[2:]:
        for anchor in ranking:
            source = anchor_file(anchor)
            if source not in files:
                selected.append(anchor)
                files.add(source)
                roles.add(anchor_role(anchor))
                break
        if len(selected) >= desired:
            return selected
    for require_new_role in (True, False):
        for anchor in ranked_anchors:
            source = anchor_file(anchor)
            role = anchor_role(anchor)
            if not anchor or source in files or (require_new_role and role in roles):
                continue
            selected.append(anchor)
            files.add(source)
            roles.add(role)
            if len(selected) >= desired:
                return selected
    return selected


def retrieve(
    aden_bin: str, repo: Path, task: dict[str, Any], budget: int, timeout: int = 30,
    routing: str = "adaptive",
) -> tuple[str, int, int, list[str]]:
    started = time.monotonic()
    actual_routing = routing_policy(task) if routing == "policy" else routing
    anchors = adaptive_anchors(aden_bin, repo, task, timeout) if actual_routing == "adaptive" else []
    if not anchors:
        anchors = []
        commands = [[aden_bin, "ask", "--human", "--strict", "--budget", str(budget), "--project", str(repo), task["question"]]]
    else:
        separator_tokens = 4 * max(0, len(anchors) - 1)
        share = max(64, (budget - separator_tokens) // len(anchors))
        commands = [
            [aden_bin, "ask", "--human", "--strict", "--budget", str(share), "--project", str(repo), "--from", anchor, task["question"]]
            for anchor in anchors
        ]
    def execute(command: list[str]) -> str:
        result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
        if result.returncode != 0:
            raise RuntimeError(result.stderr[-500:] or f"aden exited {result.returncode}")
        return result.stdout
    with ThreadPoolExecutor(max_workers=len(commands)) as pool:
        parts = list(pool.map(execute, commands))
    elapsed_ms = round((time.monotonic() - started) * 1000)
    context = "\n\n---\n\n".join(part.rstrip() for part in parts if part.strip())
    return context, elapsed_ms, (len(context.encode("utf-8")) + 3) // 4, anchors


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tasks", type=Path, default=agent_bench.DEFAULT_TASKS)
    parser.add_argument("--task", action="append")
    parser.add_argument("--aden-bin", default="aden")
    parser.add_argument("--timeout", type=int, default=30)
    parser.add_argument("--max-budget", type=int, choices=BUDGETS, default=max(BUDGETS))
    parser.add_argument(
        "--sweep-all",
        action="store_true",
        help="continue through larger budgets after the first sufficient result",
    )
    parser.add_argument("--routing", choices=["ask", "adaptive", "policy"], default="policy")
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()

    corpus = agent_bench.load_tasks(args.tasks)
    selected = [task for task in corpus["tasks"] if not args.task or task["id"] in args.task]
    records = []
    for task in selected:
        repo = agent_bench.resolve_repo(corpus["repositories"][task["repository"]])
        plan = context_plan(task["question"], task["category"])
        sweeps = []
        active_budgets = tuple(budget for budget in BUDGETS if budget <= args.max_budget)
        for budget in active_budgets:
            print(f"[context-gate] {task['id']} budget={budget}", file=__import__("sys").stderr)
            try:
                context, elapsed_ms, tokens, anchors = retrieve(
                    args.aden_bin, repo, task, budget, args.timeout, args.routing
                )
                score = score_context(task, context)
                sweeps.append({"budget": budget, "tokens": tokens, "elapsed_ms": elapsed_ms, "anchors": anchors, **score})
                # Budgets are ordered smallest-first. Routine gating needs the empirical
                # minimum, so larger calls add cost but cannot improve that decision.
                # Keep the exhaustive sweep available for research into non-monotonic
                # behavior without making every regression run pay for it.
                if score["sufficient"] and not args.sweep_all:
                    break
            except (RuntimeError, subprocess.TimeoutExpired) as error:
                sweeps.append({"budget": budget, "error": str(error), "sufficient": False})
        sufficient = [row for row in sweeps if row.get("sufficient")]
        optimal = min((row["budget"] for row in sufficient), default=None)
        eligible = [row for row in sweeps if row["budget"] <= plan.ceiling]
        selected_row = next((row for row in eligible if row.get("sufficient")), None)
        selected_budget = selected_row["budget"] if selected_row else None
        records.append(
            {
                "task_id": task["id"],
                "category": task["category"],
                "plan": asdict(plan),
                "empirical_minimum_budget": optimal,
                "policy_selected_budget": selected_budget,
                "policy_sufficient": bool(selected_row and selected_row.get("sufficient")),
                "policy_budget_overhead": None if optimal is None or selected_budget is None else selected_budget - optimal,
                "sweeps": sweeps,
            }
        )
        if args.json:
            args.json.parent.mkdir(parents=True, exist_ok=True)
            args.json.write_text(
                json.dumps({"schema_version": 1, "partial": True, "records": records}, indent=2)
                + "\n",
                encoding="utf-8",
            )

    resolved = [row for row in records if row["empirical_minimum_budget"] is not None]
    report = {
        "schema_version": 1,
        "budgets": list(active_budgets),
        "tasks": len(records),
        "summary": {
            "empirically_resolved": len(resolved),
            "policy_sufficient": sum(row["policy_sufficient"] for row in records),
            "policy_exact_minimum": sum(
                row["empirical_minimum_budget"] == row["policy_selected_budget"] for row in resolved
            ),
            "policy_over_budget": sum(
                (row["policy_budget_overhead"] or 0) > 0 for row in resolved
            ),
            "policy_under_budget": sum(
                (row["policy_budget_overhead"] or 0) < 0 for row in resolved
            ),
        },
        "records": records,
    }
    rendered = json.dumps(report, indent=2) + "\n"
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")


if __name__ == "__main__":
    main()
