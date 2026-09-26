@harness-selftest @NFR-2
Feature: The After hook detects leaked processes
  Run only by `make e2e-selftest` (STEMS_E2E_SELFTEST=1), which succeeds only
  if this scenario FAILS with a `LEAK:` message from the global After hook.
  Normal `make e2e` runs never select the @harness-selftest tag.

  Scenario: a stray process left running is reported as a leak
    Given the "minimal" workspace
    And a stray process "sleep 300" is running in a new process group
