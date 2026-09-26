@FR-CL-2 @FR-DS-1
Feature: Completions and man pages for the release packages
  release/build-extras.sh packs `stems completions <shell>` output and the
  pages written by the hidden `stems __man <outdir>` (one per command) into
  every release tarball; the Homebrew formula installs them.

  Scenario: __man writes one page per command
    Given an empty directory
    When I run "stems __man man --json"
    Then the command succeeds
    And the file "man/stems.1" exists
    And the file "man/stems-up.1" exists
    And the file "man/stems.1" contains ".TH stems 1"
    And the JSON at "$.data.pages" contains "stems-daemon-start.1"

  Scenario: fish completions are not empty
    When I run "stems completions fish"
    Then the command succeeds
    And stdout contains "complete -c stems"
    And stdout contains "upgrade"
