@slow @NFR-1
Feature: Daemon idle footprint
  An idle daemon stays small (NFR-1 allows 50 MB with 20 stems). The limit
  is a tripwire for allocation regressions, so it sits just above the
  current debug-build idle RSS, which is almost all paged-in binary (the
  heap is under 2 MB). v0.6: 17.5 MB; v0.7 (exec, build secrets, redaction,
  checks, embedded docs): 20.1 MB.

  Scenario: an idle daemon uses less than 24 MB RSS
    Given the "minimal" workspace
    And the daemon is started
    Then within 5s the events stream contains {"kind": "workspace.loaded"}
    And the daemon RSS is below 24 MB
