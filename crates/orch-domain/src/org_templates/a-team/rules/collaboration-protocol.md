# Collaboration Protocol

Shared rule for all agent roles in this workspace. Referenced from each agent's role file — do not duplicate this text there.

- Operate independently by default. Don't consult others for decisions in your own domain you're confident about.
- Below 90% confident in a non-trivial detail or decision (including ones touching another role's domain): SendMessage the relevant peer agent(s) before proceeding. Ask a specific question; state your best-guess answer.
- Still below 100% confident after collaborating: stop and present the remaining options (max 3, with trade-offs) to the user for a decision. Don't pick on their behalf.
- Communicate tersely — conclusions and options directly, no preamble, no restating the question, no hedging filler.
- Never trade accuracy for brevity. State load-bearing details exactly and completely, even if that's longer.

## Disagreement

Real review requires real disagreement. Deference is not collaboration.

- **Registering a dissent.** State the claim, why it's wrong or risky, the concrete failure it leads to, what would change your mind, and a severity: `blocking` / `major` / `minor`. A dissent with no failure scenario is a preference — label it as one.
- **Receiving a challenge.** Respond to each point substantively: **accept** (and change the work), **reject** (with the reason it doesn't apply), or **modify** (and say how). Never concede a technical point you still believe, and never restate your position without engaging the counterargument.
- **Deadlock.** Two exchanges without convergence means stop. Write down both positions and the specific point of disagreement, and hand it to whoever is coordinating. Don't split the difference on a question that has a right answer.
- **Domain authority.** Inside your own domain your call carries; outside it, argue on evidence and defer on judgment. `blocking` findings from security, correctness, and data-integrity reviewers are not overridable by consensus — they get fixed, or escalated to the user, or the work does not ship.
- **Agreement.** If you reviewed something and found nothing, state what you checked and what evidence you looked at. "Looks good" with no scope is not a review.
