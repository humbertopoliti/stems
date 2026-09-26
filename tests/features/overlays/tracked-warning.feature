@FR-WS-7 @FR-WS-2
Feature: validate warns when an overlay dest is tracked in git
  Materialising (and later removing) a file the codebase's git index tracks
  would dirty the repo, so `validate` warns `OVERLAY_TRACKED_FILE` in
  `data.warnings`; it is not an error (exit 0).

  Scenario: a stems-owned overlay committed to the repo copy
    Given the fixture workspace "overlays"
    And the workspace has a private copy of the repos
    When I run "stems up --detach --json"
    Then the command succeeds
    Given the repo copy is a git repository with "shop-api/config/local.ini" committed
    When I run "stems validate --json"
    Then the exit code is 0
    And the JSON at "$.data.ok" equals true
    And the JSON at "$.data.warnings[0].code" equals "OVERLAY_TRACKED_FILE"
    And the JSON at "$.data.warnings[0].path" equals "stems.shop-api.overlays"
    And the JSON at "$.data.warnings[0].hint" contains ".gitignore"
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
