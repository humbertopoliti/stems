@FR-ST-4
Feature: ${env.X} in the config comes from the stems command that loads it
  The daemon re-reads the config before `up`, `start`, `restart` and `run`;
  `${env.X}` is then substituted with the environment of that `stems`
  command (its caller env), not the daemon's own stale one. Variables the
  command does not set fall back to the daemon's (deliverable 49).

  Scenario: restart picks up a new value of an env reference
    Given the fixture workspace "load-caller-env"
    When I run "stems daemon start --json" with env TOKEN=old
    Then the command succeeds
    When I run "stems up api --detach --json"
    Then the command succeeds
    When I run "stems status -v --json"
    Then the JSON at "$.data.stems[0].env.T" equals "old"
    When I run "stems restart api --json" with env TOKEN=new
    Then the command succeeds
    When I run "stems status -v --json"
    Then the JSON at "$.data.stems[0].env.T" equals "new"
    When I run "stems config diff --json"
    Then the JSON at "$.data.pending" equals false
    When I run "stems down --all --json"
    Then the command succeeds
