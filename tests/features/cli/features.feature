@FR-CL-1
Feature: discover everything stems can do
  `stems features` prints the feature index (every capability with its
  commands, config keys and doc page; `--json` gives the raw table) and
  `stems docs` lists and prints the documentation pages embedded in the
  binary. `stems --help` routes to both. Neither needs a workspace.

  Scenario: stems features --json is the feature index
    Given an empty directory
    When I run "stems features --json"
    Then the command succeeds
    And the JSON at "$.data.features" has between 20 and 200 elements
    And the JSON array at "$.data.features[*].doc" has an element containing "docs/exec.md"
    And the JSON array at "$.data.features[*].commands[*]" has an element containing "stems ports"
    And the JSON array at "$.data.areas[*].id" has an element containing "scripts"
    When I run "stems features repos --json"
    Then the command succeeds
    And the JSON at "$.data.areas" has between 1 and 1 elements
    And the JSON array at "$.data.features[*].commands[*]" has an element containing "stems repos checkout"
    When I run "stems features nope --json"
    Then the JSON error has code "USAGE"

  Scenario: stems docs lists the pages and prints one
    Given an empty directory
    When I run "stems docs --json"
    Then the command succeeds
    And the JSON array at "$.data.topics[*].topic" has an element containing "features"
    And the JSON array at "$.data.topics[*].topic" has an element containing "exec"
    When I run "stems docs exec --json"
    Then the command succeeds
    And the JSON at "$.data.path" equals "docs/exec.md"
    And the JSON at "$.data.markdown" contains "stems exec"
    When I run "stems docs nope --json"
    Then the JSON error has code "USAGE"

  Scenario: stems --help routes to the feature index and the docs
    When I run "stems --help"
    Then the command succeeds
    And stdout contains "stems features"
    And stdout contains "stems docs"
    And stdout contains "Scripts & context:"
    When I run "stems exec --help"
    Then the command succeeds
    And stdout contains "See also:"
    And stdout contains "docs/exec.md"
