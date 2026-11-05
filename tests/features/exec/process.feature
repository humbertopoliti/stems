@FR-LC-7
Feature: stems exec runs a command in a process stem's context
  `stems exec <stem> -- <cmd>` runs the command in the caller's terminal
  with the stem's environment (PORT, env, STEMS_*) in its codebase and
  exits with the command's exit code. Without a daemon the context comes
  from the config, and no daemon is started (deliverable 37).

  Scenario: the stem's port, name and codebase; the command's exit code
    Given the fixture workspace "caller-env"
    When I run "stems up api --detach --json"
    Then the command succeeds
    When I run "stems exec api -- sh -c 'echo $PORT:$STEMS_STEM:$GREETING:$(pwd -P)'"
    Then the command succeeds
    And stdout contains "${port:18791}:api:hello:"
    And stdout contains "examples/repos/shop-api"
    When I run "stems exec api -- sh -c 'exit 7'"
    Then the exit code is 7
    When I run "stems exec api --json -- true"
    Then the JSON at "$.data.exit" equals 0
    And the JSON at "$.data.container" equals null
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: without a daemon exec reads the config and starts nothing
    Given the fixture workspace "caller-env"
    When I run "stems exec api -- sh -c 'echo stem=$STEMS_STEM port=$PORT'" with env FOO=x
    Then the command succeeds
    And stdout contains "stem=api port=${port:18791}"
    When I run "stems exec worker -- sh -c 'echo url=$URL port=${PORT-unset}'"
    Then the command succeeds
    And stdout contains "port=unset"
    And stdout contains "url=http://localhost:$"
    When I run "stems daemon status --json"
    Then the JSON at "$.data.running" equals false

  Scenario: --fresh keeps only the minimal environment
    Given the fixture workspace "caller-env"
    When I run "stems exec api --fresh -- sh -c 'echo FOO=${FOO-unset} PATH=${PATH:+set}'" with env FOO=x
    Then the command succeeds
    And stdout contains "FOO=unset PATH=set"

  Scenario: exec gets the real value of a dependency's secret output; stems env redacts it
    Given the fixture workspace "outputs-demo"
    When I run "stems up web --detach --json"
    Then the command succeeds
    When I run "stems exec web -- sh -c 'echo token=$STEMS_API_OUTPUT_TOKEN'"
    Then the command succeeds
    And stdout contains "token=tok-${port:18481}"
    When I run "stems env web --json"
    Then the JSON at "$.data.env.STEMS_API_OUTPUT_TOKEN" equals "<redacted>"
    When I run "stems down --all --json"
    Then the command succeeds

  @error
  Scenario: an unknown stem
    Given the fixture workspace "caller-env"
    When I run "stems exec nope --json -- true"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_STEM"
