@FR-CL-2
Feature: Shell completions
  `stems completions <bash|zsh|fish>` prints the script itself (no envelope)
  so it can be redirected into a completions directory.

  Scenario: zsh completions
    When I run "stems completions zsh"
    Then the command succeeds
    And stdout contains "#compdef stems"
    And stdout contains "_stems"

  Scenario: bash completions
    When I run "stems completions bash"
    Then the command succeeds
    And stdout contains "_stems"

  Scenario: fish completions
    When I run "stems completions fish"
    Then the command succeeds
    And stdout contains "complete -c stems"

  Scenario: --json wraps the script in the envelope
    When I run "stems completions zsh --json"
    Then the command succeeds
    And the JSON at "$.data.shell" equals "zsh"
    And the JSON at "$.data.script" contains "_stems"
