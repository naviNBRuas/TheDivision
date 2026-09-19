# TDD Workflow

Write the test before the implementation, for any feature or bug fix.

1. Write a test that fails for the right reason. Run it and confirm the failure — a
   test that passes before the code exists is testing nothing.
2. Write the minimum code to make it pass. Resist adding anything the test doesn't
   require yet.
3. Refactor with the test as a safety net. Re-run it after every change.
4. Repeat for the next behavior.

For a bug fix specifically: the failing test should reproduce the bug itself, not
just exercise the surrounding code. If you can't write a test that fails against the
buggy code, you don't yet understand the bug.

Skip this loop only for pure exploration/spikes you intend to throw away — not for
anything that will ship.
