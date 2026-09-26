@FR-WS-3
Feature: git codebases are cloned on the first up
  A stem whose `codebase` is `{ git: <url>, ref: <ref> }` is cloned into
  `.stems/repos/<stem>` the first time `up` needs it, before its setup, and
  then starts from the clone. `up` never fetches an existing clone (only
  `up --sync` and `repos sync` do). git's output goes to the stem's log with
  `tag: git`.

  Scenario: up clones the missing codebase, the second up does not fetch
    Given the fixture workspace "git-codebase"
    And a bare git repository "shop" made from "examples/repos/shop-api"
    And a local override setting stems.shop-api.codebase.git=${var:shop_url}
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" contains "shop-api"
    And the file ".stems/repos/shop-api/app.py" exists
    And the events stream contains {"kind": "repo.cloned", "stem": "shop-api", "data": {"ref": "main"}} before {"kind": "stem.state", "stem": "shop-api", "to": "starting"}
    When I run "stems logs shop-api --json"
    Then the JSON at "$[?@.tag == 'git'].text" contains "git clone --branch main"
    When I run "stems repos status --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].stem" equals "shop-api"
    And the JSON at "$.data.repos[0].source" equals "git"
    And the JSON at "$.data.repos[0].exists" equals true
    And the JSON at "$.data.repos[0].branch" equals "main"
    And the JSON at "$.data.repos[0].dirty" equals false
    And the JSON at "$.data.repos[0].ahead" equals 0
    And the JSON at "$.data.repos[0].behind" equals 0
    When I run "stems up --detach --json"
    Then the command succeeds
    And the events stream does not contain {"kind": "repo.fetched"}
    And the events stream does not contain {"kind": "repo.failed"}
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: up --sync fetches the existing clone before starting
    Given the fixture workspace "git-codebase"
    And a bare git repository "shop" made from "examples/repos/shop-api"
    And a local override setting stems.shop-api.codebase.git=${var:shop_url}
    When I run "stems repos sync --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].action" equals "cloned"
    When I run "stems up --sync --detach --json"
    Then the command succeeds
    And the events stream contains {"kind": "repo.fetched", "stem": "shop-api", "data": {"updated": false}} before {"kind": "stem.state", "stem": "shop-api", "to": "starting"}
    When I run "stems down --all --json"
    Then the command succeeds
