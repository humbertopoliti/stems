@FR-SC-3 @FR-ST-4
Feature: what a request starts gets the environment of the `stems` command
  The daemon keeps its own environment from when it started, but scripts,
  processes and command probes a request starts get the caller's
  environment (minus STEMS_*) as their base layer (deliverable 37). A
  script with `inherit_env: false` gets only PATH, HOME, USER, LOGNAME,
  SHELL, TMPDIR, TERM, LANG and LC_* of it, under what stems sets.

  Scenario: scripts and starts see the caller's environment, not the daemon's
    Given the fixture workspace "caller-env"
    When I run "stems daemon start --json" with env FOO=stale
    Then the command succeeds
    When I run "stems up api --detach --json" with env FOO=up
    Then the command succeeds
    When I run "stems logs api --json"
    Then the JSON at "$..text" contains "started FOO=up"
    When I run "stems run api print-env --json" with env FOO=fresh
    Then the command succeeds
    And the JSON at "$.data.tail[0]" equals "FOO=fresh"
    When I run "stems run api print-env --json"
    Then the command succeeds
    And the JSON at "$.data.tail[0]" equals "FOO=unset"
    When I run "stems restart api --json" with env FOO=restarted
    Then the command succeeds
    When I run "stems logs api --json"
    Then the JSON at "$..text" contains "started FOO=restarted"
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: a script with inherit_env false sees PATH and PORT but not FOO
    Given the fixture workspace "caller-env"
    When I run "stems run api clean --json" with env FOO=fresh
    Then the command succeeds
    And the JSON at "$.data.tail[0]" equals "FOO=unset PATH=set PORT=${port:18791} GREETING=hello"
    When I run "stems scripts api --json"
    Then the JSON at "$.data.scripts[?@.name == 'clean'].inherit_env" equals false
    And the JSON at "$.data.scripts[?@.name == 'print-env'].inherit_env" equals true

  Scenario: build and restart --build run in the caller's environment
    Given the fixture workspace "caller-env"
    When I run "stems daemon start --json" with env FOO=stale
    Then the command succeeds
    When I run "stems build api --json" with env FOO=built
    Then the command succeeds
    When I run "stems logs api --json"
    Then the JSON at "$..text" contains "build FOO=built"
    When I run "stems up api --detach --json" with env FOO=up
    Then the command succeeds
    When I run "stems restart api --build --json" with env FOO=rebuilt
    Then the command succeeds
    When I run "stems logs api --json"
    Then the JSON at "$..text" contains "build FOO=rebuilt"
    And the JSON at "$..text" contains "started FOO=rebuilt"
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: a restart stems makes on its own keeps the caller's environment and --pass-env
    Given the fixture workspace "caller-env"
    When I run "stems daemon start --json" with env FOO=stale
    Then the command succeeds
    When I run "stems up api --detach --pass-env BAR --json" with env FOO=up BAR=passed
    Then the command succeeds
    Given a local override setting stems.api.env.MARK=applied
    When I run "stems config apply --yes --json"
    Then the command succeeds
    And the JSON at "$.data.applied[?@.stem=='api'].result" equals "restarted"
    When I run "stems logs api --json"
    Then the JSON at "$..text" contains "started FOO=up BAR=passed MARK=applied"
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: restart keeps the --pass-env set of the last start unless --pass-env is given
    Given the fixture workspace "caller-env"
    When I run "stems daemon start --json" with env FOO=stale
    Then the command succeeds
    When I run "stems up api --detach --pass-env BAR --json" with env FOO=up BAR=passed
    Then the command succeeds
    When I run "stems restart api --json" with env FOO=again BAR=shell
    Then the command succeeds
    When I run "stems logs api --json"
    Then the JSON at "$..text" contains "started FOO=again BAR=passed MARK=unset"
    When I run "stems restart api --pass-env BAR --json" with env FOO=third BAR=fresh
    Then the command succeeds
    When I run "stems logs api --json"
    Then the JSON at "$..text" contains "started FOO=third BAR=fresh MARK=unset"
    When I run "stems down --all --json"
    Then the command succeeds
