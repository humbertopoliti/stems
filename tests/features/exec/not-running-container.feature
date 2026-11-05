@docker @FR-LC-7
Feature: stems exec into a container stem
  A docker/compose stem's command runs in its container (`docker exec`);
  a stopped one is STEM_NOT_RUNNING (deliverable 37).

  Scenario: stopped, then running
    Given the fixture workspace "compose-redis"
    When I run "stems exec cache --json -- redis-cli ping"
    Then the exit code is 1
    And the JSON error has code "STEM_NOT_RUNNING"
    And the JSON at "$.errors[0].hint" contains "--host"
    When I run "stems up cache --detach --json"
    Then the command succeeds
    When I run "stems exec cache -- redis-cli ping"
    Then the command succeeds
    And stdout contains "PONG"
    When I run "stems down --all --json"
    Then the command succeeds
