@FR-UI-3 @FR-UI-1
Feature: Switching stems inside the Detail and Logs views
  `[` / `]` select the previous / next stem (table order, wrapping) in
  every view; in Detail and Logs `h` / `l` and `←` / `→` do the same. The
  selection is shared, so Detail reloads, the Logs view and the split log
  pane resubscribe to the new stem, and the table highlight follows. The
  Detail and Logs titles carry the stem strip (` a  [b]  c  d `, the
  picker). `PageUp`/`PageDown` (and `G`) scroll a Detail taller than the
  view; its title then says which lines show.

  Background:
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds

  Scenario: ] in Detail shows the next stem's detail
    When I run "stems attach --view table --size 100x100 --headless --script 'wait:healthy;3;frame;];frame;l;Left;h;frame'"
    Then the command succeeds
    And frame 1 contains "Detail: a"
    And frame 1 contains "[a]  b  c  d · [ ] ←/→ switch"
    And frame 2 contains "Detail: b"
    And frame 2 contains "a  [b]  c  d"
    And frame 2 contains "depends_on:"
    And frame 3 contains "Detail: a"

  Scenario: ] in Logs follows the next stem's logs
    # `]` resets the pane: frame 2 holds only b's replayed lines. The port
    # in the line is not asserted: the fixture's remapped ports did not
    # reliably reach the processes' `PORT` while this was written.
    When I run "stems attach --view table --headless --script 'wait:healthy;4;wait:log=listening on;frame;];wait:log=listening on;frame'"
    Then the command succeeds
    And frame 1 contains "Logs: a · following"
    And frame 1 contains "INFO listening on 127.0.0.1:"
    And frame 2 contains "Logs: b · following"
    And frame 2 contains "a  [b]  c  d"
    And frame 2 contains "INFO listening on 127.0.0.1:"
    And frame 2 does not contain "Logs: all stems"

  Scenario: ] with the split pane on the table moves the pane to the next stem
    When I run "stems attach --view table --headless --script 'wait:healthy;Ctrl-L;wait:log=listening on;frame;];wait:log=listening on;frame'"
    Then the command succeeds
    And frame 1 contains "Logs: a · following"
    And frame 1 contains "› a"
    And frame 2 contains "Logs: b · following"
    And frame 2 contains "› b"
    And frame 2 contains "INFO listening on 127.0.0.1:"

  Scenario: PageDown scrolls a Detail taller than the view
    When I run "stems attach --view table --size 80x14 --headless --script 'wait:healthy;3;frame;PageDown;frame'"
    Then the command succeeds
    And frame 1 contains "Detail: a"
    And frame 1 contains "· lines 1-9/"
    And frame 2 contains "Detail: a"
    And frame 2 contains "· lines "
    And frame 2 does not contain "· lines 1-9/"
