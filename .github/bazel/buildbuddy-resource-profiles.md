# BuildBuddy resource profile pilot

Status: the repository variable `CODEX_BUILDBUDDY_RESOURCE_PROFILE` is set to
`large` for the x86_64 pilot at the user's explicit request. ARM64 stays on `auto`.
The helper still defaults to `auto` when no profile is specified. Included
account capacity and current per-action allocations remain unverified; GitHub
Actions holds the API key as a secret unavailable to the implementation
environment. The pilot starts before the capacity check and paired benchmarks
are complete; enabling it is not a measured acceptance result.

## Configuration

Set the optional repository variable `CODEX_BUILDBUDDY_RESOURCE_PROFILE` to
`large` to request 8 CPUs and 32 GB per remote action on x86_64 Linux platform
test jobs. ARM64 remains on `auto`. Unset the variable or set it to `auto` to
roll back. Runner sizes, Bazel concurrency, and local test concurrency are
unchanged. Local calls to `run_bazel_with_buildbuddy.py` accept the same environment
variable. Without credentials the helper retains local fallback.

The helper forwards the validated profile as
`--repo_env=CODEX_RBE_RESOURCE_PROFILE=auto|large` only for authenticated RBE.
The repository rule reads it with `repository_ctx.getenv`, so changes invalidate
the generated platform. `auto` adds no resource properties; `large` adds
`EstimatedCPU=8` and `EstimatedMemory=32GB`, preserving both architectures' images
and constraints. These are manual allocation overrides described in the
[BuildBuddy execution properties documentation](https://www.buildbuddy.io/docs/rbe-platforms/#runner-resource-allocation).
They are not guaranteed to increase automatic allocations.

## Baseline observed on 2026-10-15

[Successful baseline invocation](https://app.buildbuddy.io/invocation/6cfa3c41-d4a2-4d46-9c61-6570dff84313),
revision `3f4ea795c2b52baaf88d4807d233287db5d03068`, Bazel 9.2.0.
Measurements were read from the public `GetInvocation` and `GetExecution` RPCs.

| Measurement                                              | Baseline                                                           |
| -------------------------------------------------------- | ------------------------------------------------------------------ |
| Bazel invocation duration (not full GitHub job duration) | 2,085.335 seconds                                                  |
| Overall critical path                                    | 2,012.883 seconds                                                  |
| Compilation actions on the reported critical path        | 838.68 seconds                                                     |
| Queue time within those compilation actions              | Approximately 0.395 seconds                                        |
| Queue time across all 31 reported remote executions      | 2.122 seconds total; 0.0667 seconds median; 0.1182 seconds maximum |
| Action cache                                             | 5,260 hits, 110 misses (97.95% hit rate)                           |
| CAS cache                                                | 178,638 hits                                                       |
| Allocated CPU and RAM                                    | Not exposed by the retrieved execution metadata; unverified        |
| Included account capacity                                | Requires authenticated account access; unverified                  |

Compilation time is the sum of the 45 `Compiling` action durations in Bazel's
critical-path report, including input setup and cache-hit overhead. Its queue
time is estimated from the rounded percentages in that report. The path also
includes a 1,171.69-second TUI test shard. These observations alone do not establish
that increased CPU or memory will improve the job.

## Acceptance and observation

Confirm that 8 CPUs and 32 GB fit the current account's included capacity and do
not reduce the current allocations. Roll back if either condition fails. Do not
change subscription tiers or provision executor pools.

Warm each profile separately, then collect at least three paired builds of the
same revision and target set with comparable cache states. Exclude fully cached
compilation and network-failure runs. Record invocation links, compilation
critical path, queue times, allocated CPU/RAM, cache hits, full job duration, and
account usage for every eligible run. No eligible pairs have been collected yet;
there is no measured improvement or acceptance decision.

Keep `large` enabled only when median compilation critical-path time improves by at
least 20%, total job duration regresses by no more than 5%, all existing tests
pass, and included capacity is sufficient. Observe the first five successful
x86_64 jobs for queueing, resource failures, duration, and usage. Validate ARM64
separately before expanding the workflow selection. Roll back to `auto` if the
acceptance conditions stop holding. Connection timeouts remain a separate issue.
