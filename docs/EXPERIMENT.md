# State as Context Evidence

This document records the first validation of the architecture. It is
evidence, not a production guarantee.

## Experiment

The experiment used a real completed AgentHub task and intentionally reset the
model conversation before each recovery decision. The same `gpt-5.6-sol` model
with high reasoning was used for all six invocations: two equivalent recovery
questions for each of three context conditions.

The nine deterministic checks were:

- goal recovered;
- task status correctly identified as done;
- exact accepted Artifact path;
- acceptance recognized;
- original research not repeated;
- next action does not restart completed work;
- evidence references bound;
- at least two failure categories recovered;
- context compaction recognized.

Token count was recorded but was not a quality score.

## Conditions and result

| Condition | Input size | Scores |
| --- | ---: | ---: |
| raw event and semantic history | about 109k input tokens | `9/9`, `8/9` |
| current AgentHub `ContextEngine` projection | about 15k input tokens | `9/9`, `9/9` |
| quality-first State + Evidence projection | about 22k input tokens | `9/9`, `9/9` |

The raw-history second trial classified the already completed task as
`in_progress`. The current ContextEngine projection and the rich State
projection both recovered the task correctly in both trials.

## What this supports

- A finite State projection can preserve the information needed to resume a
  long task after a complete conversation reset.
- More transcript is not automatically more reliable context.
- The central mechanism is State plus Evidence projection, not the number of
  Agents, Bubbles, or roles.
- Context size can be substantially smaller without making quality-first
  recovery worse in this sample.

## What this does not prove

- It is one task, one model, and two trials per condition.
- The raw-history condition was an auditable event proxy, not a complete Pi or
  Codex transcript.
- The three conditions shared the final Artifact to avoid mixing artifact
  readability into the comparison.
- It did not prove concurrent stale-result handling, process restart, outbox
  replay, capability safety, unfinished-task recovery, or cross-task quality.

The full report and structured answers remain in the AgentHub experiment
directory. Anchor treats this result as a reason to build and test the runtime,
not as permission to claim universal long-task stability.
