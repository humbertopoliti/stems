@FR-DS-2
Feature: stems upgrade
  `stems upgrade` upgrades stems the way it was installed: under a Homebrew
  Cellar it runs `brew upgrade stems`; an installer.sh/tarball install gets
  the installer one-liner; a `cargo install` gets the cargo command.
  `--dry-run` never runs anything. `STEMS_FAKE_INSTALL_METHOD` overrides the
  detection (never run these without --dry-run: brew would really run).

  Scenario: a brew install upgrades with brew
    When I run "stems upgrade --dry-run --json" with env STEMS_FAKE_INSTALL_METHOD=brew
    Then the command succeeds
    And the JSON at "$.data.install_method" equals "brew"
    And the JSON at "$.data.command" equals "brew upgrade stems"
    And the JSON at "$.data.ran" equals false

  @FR-DS-3
  Scenario: a tarball install is told to re-run installer.sh
    When I run "stems upgrade --dry-run --json" with env STEMS_FAKE_INSTALL_METHOD=tarball
    Then the command succeeds
    And the JSON at "$.data.install_method" equals "tarball"
    And the JSON at "$.data.command" contains "installer.sh"
    And the JSON at "$.data.command" contains "releases/latest/download"

  Scenario: a tarball install only prints the command, even without --dry-run
    When I run "stems upgrade --json" with env STEMS_FAKE_INSTALL_METHOD=tarball
    Then the command succeeds
    And the JSON at "$.data.ran" equals false
    And the JSON at "$.data.command" contains "installer.sh"

  Scenario: a cargo install is told to cargo install
    When I run "stems upgrade --dry-run --json" with env STEMS_FAKE_INSTALL_METHOD=cargo
    Then the command succeeds
    And the JSON at "$.data.install_method" equals "cargo"
    And the JSON at "$.data.command" contains "cargo install"

  Scenario: --version --json reports the install method
    When I run "stems --version --json" with env STEMS_FAKE_INSTALL_METHOD=brew
    Then the command succeeds
    And the JSON at "$.data.install_method" equals "brew"
