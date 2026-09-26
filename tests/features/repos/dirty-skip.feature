@FR-WS-3 @NFR-3
Feature: repos sync never touches local work
  A clone with uncommitted changes (or with HEAD moved to another branch by
  the developer) is reported `skipped_dirty` and left exactly as it is.

  Scenario: a modified clone is skipped and the file is untouched
    Given the fixture workspace "git-codebase"
    And a bare git repository "shop" made from "examples/repos/shop-api"
    And a local override setting stems.shop-api.codebase.git=${var:shop_url}
    When I run "stems repos sync --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].action" equals "cloned"
    Given the file ".stems/repos/shop-api/app.py" is written with "# my local work"
    When I run the shell command "cd ${var:shop_src} && echo 2.0.0 > VERSION && git add VERSION && git commit -q -m v2 && git tag v2 && git push -q origin main v2"
    Then the command succeeds
    Given a local override setting stems.shop-api.codebase.ref=v2
    When I run "stems repos sync --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].action" equals "skipped_dirty"
    And the JSON at "$.data.repos[0].message" contains "uncommitted"
    And the file ".stems/repos/shop-api/app.py" contains "# my local work"
    And the file ".stems/repos/shop-api/VERSION" does not exist
    When I run "stems repos status --json"
    Then the JSON at "$.data.repos[0].dirty" equals true
    And the JSON at "$.data.repos[0].branch" equals "main"

  Scenario: a clone switched to another branch is skipped
    Given the fixture workspace "git-codebase"
    And a bare git repository "shop" made from "examples/repos/shop-api"
    And a local override setting stems.shop-api.codebase.git=${var:shop_url}
    When I run "stems repos sync --json"
    Then the command succeeds
    When I run the shell command "cd .stems/repos/shop-api && git checkout -q -b my-feature"
    Then the command succeeds
    When I run "stems repos sync --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].action" equals "skipped_dirty"
    And the JSON at "$.data.repos[0].message" contains "my-feature"
    When I run "stems repos status --json"
    Then the JSON at "$.data.repos[0].branch" equals "my-feature"
