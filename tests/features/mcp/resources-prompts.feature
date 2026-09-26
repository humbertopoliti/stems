@FR-AI-1
Feature: MCP resources and prompts
  Resources: `stems://workspace`, `stems://graph`, `stems://<stem>/config`
  and `stems://<stem>/logs?tail=N`. Prompts: `diagnose_stem {stem}` and
  `bring_up_and_report {profile?}`.

  Scenario: read a stem's logs and config, and diagnose it
    Given the "minimal" workspace is up
    When the chaos endpoint "logs?n=3&level=error" is called on "echo-svc"
    Then the chaos response status is 200
    And within 5s the stem log file "echo-svc/current.log" contains "chaos log line 2"
    Given an MCP client is connected
    When the MCP client reads resource "stems://echo-svc/logs?tail=5"
    Then the resource content contains "chaos log line 2"
    When the MCP client reads resource "stems://echo-svc/config"
    Then the resource content contains "SHOP_CHAOS"
    When the MCP client reads resource "stems://workspace"
    Then the resource content contains "echo-svc"
    When the MCP client reads resource "stems://graph"
    Then the resource content contains "healthy"
    When the MCP client gets prompt "diagnose_stem" with {"stem": "echo-svc"}
    Then the prompt text contains "Diagnose the stem `echo-svc`"
    And the prompt text contains "state: healthy"
    And the prompt text contains "chaos log line 2"
    And the prompt text contains "## What to do"
    When the MCP client gets prompt "bring_up_and_report" with {}
    Then the prompt text contains "echo-svc: healthy"
    When I run "stems down --json"
    Then the command succeeds
