import argparse
import json
import pathlib
import re
import statistics


PRIORITY = re.compile(r"\[P([0-3])\]")
ASSESSMENT = re.compile(
    r"Assessment(?: before fixes)?\s*:?\s*"
    r"(patch is correct|patch is incorrect|uncertain)",
    re.IGNORECASE,
)


def words(text):
    return len(re.findall(r"\S+", text or ""))


def normalized(text):
    return re.sub(r"[^a-z0-9]+", " ", (text or "").lower()).strip()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", required=True)
    parser.add_argument("--results", required=True)
    args = parser.parse_args()
    corpus = json.loads(pathlib.Path(args.corpus).read_text(encoding="utf-8"))
    artifact = json.loads(pathlib.Path(args.results).read_text(encoding="utf-8"))
    rows = artifact["results"]
    summary = {}
    for arm in ["main", "singlePass", "doubleCheck"]:
        arm_rows = [row for row in rows if row["arm"] == arm]
        true_positive = false_positive = false_negative = 0
        exact_priority = within_one_priority = 0
        classifications = classification_total = duplicates = correct_verdicts = 0
        correct_finding_decisions = 0
        for row in arm_rows:
            oracle = corpus[row["case"]]
            report = row["report"] or ""
            count = len(PRIORITY.findall(report))
            report_normalized = normalized(report)
            root_cause_terms = sum(
                normalized(term) in report_normalized
                for term in oracle["rootCauseTerms"]
            )
            root_cause_match = root_cause_terms >= min(
                2, len(oracle["rootCauseTerms"])
            )
            if oracle["expectedFinding"]:
                if count and root_cause_match:
                    true_positive += 1
                    false_positive += max(0, count - 1)
                else:
                    false_negative += 1
                    false_positive += count
                match = PRIORITY.search(report)
                if count and root_cause_match and match:
                    delta = abs(int(match.group(1)) - oracle["priority"])
                    exact_priority += delta == 0
                    within_one_priority += delta <= 1
                if arm == "doubleCheck":
                    classification_total += 1
                    if "Pre-existing: no" in report:
                        classifications += 1
            else:
                false_positive += count
            if bool(count) == oracle["expectedFinding"]:
                correct_finding_decisions += 1
            expected_verdict = (
                "patch is incorrect" if oracle["expectedFinding"] else "patch is correct"
            )
            verdict = ASSESSMENT.search(report)
            if verdict and verdict.group(1).lower() == expected_verdict:
                correct_verdicts += 1
            maximum = oracle.get("maxFindings")
            if maximum is not None and count > maximum:
                duplicates += count - maximum
        precision = true_positive / max(1, true_positive + false_positive)
        recall = true_positive / max(1, true_positive + false_negative)
        f1 = 2 * precision * recall / max(precision + recall, 1e-9)
        summary[arm] = {
            "truePositives": true_positive,
            "falsePositives": false_positive,
            "falseNegatives": false_negative,
            "precision": round(precision, 3),
            "recall": round(recall, 3),
            "f1": round(f1, 3),
            "exactP1": exact_priority,
            "priorityWithinOne": within_one_priority,
            "correctFindingDecisions": correct_finding_decisions,
            "correctVerdicts": None if arm == "main" else correct_verdicts,
            "introducedClassifications": classifications,
            "classificationTotal": classification_total,
            "duplicates": duplicates,
            "meanWallSeconds": round(
                statistics.mean(row["elapsedSeconds"] for row in arm_rows), 3
            ),
            "totalReportWords": sum(words(row["report"]) for row in arm_rows),
        }
    print(json.dumps({"manifest": artifact["manifest"], "summary": summary}, indent=2))


if __name__ == "__main__":
    main()
