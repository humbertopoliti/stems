@FR-WD-4
Feature: doctor warns when the watchdog and a dev server's hot reload overlap
  A stem whose start command already hot-reloads (`vite`, `next dev`,
  `nodemon`, `cargo watch`, `air`, `uvicorn --reload`, ...) and that also
  has a `watch` on its sources (`src/**` or `**`) would be restarted on
  every save: `stems doctor` warns `watch.hot-reload.<stem>`.

  Scenario: vite plus a watch on src/**
    Given the fixture workspace "hot-reload"
    And a fake tool "vite" on PATH that runs "echo fake vite"
    When I run "stems doctor --json"
    Then the command succeeds
    And the JSON at "$.data.checks[?@.id == 'watch.hot-reload.web'].status" equals "warn"
    And the JSON at "$.data.checks[?@.id == 'watch.hot-reload.web'].message" contains "vite"
    And the JSON at "$.data.checks[?@.id == 'watch.hot-reload.web'].details.paths" equals ["src/**"]
    And the JSON at "$.data.checks[?@.id == 'watch.hot-reload.api'].status" equals "ok"
    When I run "stems doctor --strict --json"
    Then the exit code is 1
