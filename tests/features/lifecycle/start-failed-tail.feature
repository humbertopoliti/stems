@FR-LC-6 @error
Feature: a failed start carries the last output lines
  When a process exits before it is ready (`START_FAILED`) the error's
  details carry `stderr_tail` (the last stderr lines of the attempt that
  died, oldest first, always present), `stdout_tail` only when stderr was
  silent, and `output_truncated`. `up` and `status` report the same error,
  so a client sees why without a `stems logs` round trip.

  Scenario: stderr tail, and the stdout tail only when stderr was silent
    Given the fixture workspace "start-failed-tail"
    When I run "stems up --detach --no-fail-fast --json"
    Then the exit code is 1
    And the JSON at "$.data.failed[?@.stem == 'boom'].error.code" equals "START_FAILED"
    And the JSON at "$.data.failed[?@.stem == 'boom'].error.details.stderr_tail" equals ["boom"]
    And the JSON at "$.data.failed[?@.stem == 'boom'].error.details.stdout_tail" does not exist
    And the JSON at "$.data.failed[?@.stem == 'boom'].error.details.output_truncated" equals false
    And the JSON at "$.data.failed[?@.stem == 'quiet'].error.code" equals "START_FAILED"
    And the JSON at "$.data.failed[?@.stem == 'quiet'].error.details.stderr_tail" equals []
    And the JSON at "$.data.failed[?@.stem == 'quiet'].error.details.stdout_tail" equals ["only-stdout"]
    And the JSON at "$.data.failed[?@.stem == 'quiet'].error.details.output_truncated" equals false
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name == 'boom'].state" equals "failed"
    And the JSON at "$.data.stems[?@.name == 'boom'].error.details.stderr_tail" equals ["boom"]
    And the JSON at "$.data.stems[?@.name == 'quiet'].error.details.stdout_tail" equals ["only-stdout"]
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
