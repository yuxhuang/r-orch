A callback panic rolls back its SQLite transaction without stopping the writer; failed savepoint cleanup preserves the original failure causes before stopping it.
