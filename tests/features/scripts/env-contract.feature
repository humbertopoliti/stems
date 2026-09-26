@FR-SC-3
Feature: the environment of a stem's scripts
  Scripts get the stem's resolved environment plus the STEMS_* context:
  STEMS_STEM, STEMS_CODEBASE, STEMS_WORKSPACE (the integration repo),
  STEMS_RUN_ID, STEMS_STATE_DIR (created, per stem, under STEMS_HOME),
  STEMS_SCRIPT, STEMS_<DEP>_PORT for each dependency and PORT for the stem's
  own primary port. They run in the codebase.

  Scenario: a setup script sees the env contract
    Given the fixture workspace "scripts-env"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems logs api --script setup --json"
    Then the JSON at "$[*].text" contains "STEMS_STEM=api"
    And the JSON at "$[*].text" contains "STEMS_CODEBASE=${tmp}/examples/repos/shop-api"
    And the JSON at "$[*].text" contains "STEMS_WORKSPACE=${ws}"
    And the JSON at "$[*].text" contains "STEMS_STATE_DIR=${home}/"
    And the JSON at "$[*].text" contains "/stems/api"
    And the JSON at "$[*].text" contains "STEMS_SCRIPT=setup"
    And the JSON at "$[*].text" contains "STEMS_RUN_ID="
    And the JSON at "$[*].text" contains "STEMS_POSTGRES_PORT=${port:18551}"
    And the JSON at "$[*].text" contains "PORT=${port:18552}"
    And the JSON at "$[*].text" contains "API_MODE=contract"
    And the JSON at "$[*].text" contains "cwd="
    And the JSON at "$[*].text" contains "examples/repos/shop-api"
    When I run "stems down --all --json"
    Then the command succeeds
