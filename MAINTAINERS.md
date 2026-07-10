= Maintainers

[[bdfl]]
== BDFL / Original Author

RioPlay <rioplay@rioplay.dev>

Project creator, original author, and architectural final authority.

== Governance Model

BDFL (Benevolent Dictator For Life) with DCO-gated contributions.
All contributions must include a `Signed-off-by` line and pass structural validation via `aden check`.

[[ownership]]
== Ownership and Review

The BDFL is the default maintainer for every repository surface unless a future
maintainer is explicitly added to this file. An area without a named additional
owner is *not* unmaintained: it remains subject to the normal contribution and
review process, and contributors must not imply otherwise in issues or pull
requests.

Changes to the following protected surfaces require documented maintainer review
and the applicable validation before merge:

* public CLI, MCP, graph, store, or result-schema compatibility;
* graph/store migrations and their rollback paths;
* authentication, secret handling, path confinement, command execution, or
  vulnerability remediation; and
* governance, licensing, contributor, or release-policy documents.

The BDFL may approve and merge protected changes after recording the affected
contract, validation, and recovery or rollback path in the pull request or
issue. When an independent maintainer is formally recorded here, that person
must review protected changes they did not author. Until then, independent
review is encouraged but is not a release or merge gate: no tool, agent, or
process acting on the BDFL's direction counts as an independent reviewer.

For an active security incident, critical release breakage, or credible risk of
data loss, the BDFL may take the smallest reversible action necessary to
contain harm and record the rationale and follow-up work afterward.

[[ownership-matrix]]
=== Ownership matrix

This matrix makes the present single-maintainer posture explicit.  "Vacant" is
not a request for a contributor to act; it means the project has not received
and recorded a willing person's consent for that role.  The BDFL is the current
accountable owner for every listed surface.  No listed surface has a named
independent owner or reviewer today.

|===
|Surface |Current accountable owner |Independent owner/reviewer |Status

|`aden-cli`, installation, configuration, and release artifacts
|BDFL
|None appointed; BDFL approval after documented validation
|Single-maintainer

|`aden-mcp` and published MCP schemas
|BDFL
|None appointed; BDFL approval after documented validation
|Single-maintainer

|`aden-lsp` and editor-facing experimental interfaces
|BDFL
|None appointed; BDFL approval after documented validation
|Single-maintainer

|`aden-core`, `aden-asm`, `aden-emit`, `aden-parse`, `aden-paths`, and `aden-policy`
|BDFL
|None appointed; BDFL approval after documented validation
|Single-maintainer

|`aden-graph`, `aden-index`, `aden-store`, persisted schemas, and migrations
|BDFL
|None appointed; BDFL approval after documented validation
|Single-maintainer

|`aden-diagnose`, `aden-heal`, and `aden-propose`
|BDFL
|None appointed; BDFL approval after documented validation
|Single-maintainer

|Security policy, vulnerability intake, and incident response
|BDFL
|Backup responder: none appointed; BDFL is the active responder
|Single point of contact; see <<continuity>>

|Governance, licensing, contributor policy, and compatibility policy
|BDFL
|None appointed; BDFL approval after documented validation
|Single-maintainer
|===

The matrix is deliberately an ownership record, not a delegation mechanism.
Adding or changing a row requires the named person's explicit consent and an
update to this file in the same reviewed change.

[[emergency-authority]]
== Emergency Authority

For an active security incident, critical release breakage, or credible risk of
data loss, the BDFL may take the smallest reversible action necessary to contain
harm. Emergency authority is not a standing exception to normal review,
compatibility, or disclosure practices. The incident record must identify the
trigger, changes made, rollback plan, and the follow-up review needed to return
to normal governance.

[[continuity]]
== Continuity and Succession

The project does not currently name a backup maintainer or security responder.
That absence is intentional and visible; no person is assigned responsibility
without their explicit consent and an update to this file.

If the BDFL expects to be unavailable for a material period, they should publish
an issue or release note identifying the expected response posture and any
consenting temporary delegates. A delegate's authority, duration, and permitted
surfaces must be stated explicitly.

If the BDFL becomes unreachable, contributors may continue proposing changes,
but must not represent the project as actively maintained, publish releases, or
change protected surfaces without a documented transfer of authority. A future
successor or maintainer group must be named here, accept the role explicitly,
and preserve the project license and contributor commitments before exercising
maintainer authority.

[[decision-appeal]]
== Decision Appeal

Anyone affected by a maintainer decision may request reconsideration in a
public issue or pull request, with the decision, the requested outcome, and the
evidence for it.  The BDFL should respond with a durable record: acceptance,
rejection with rationale, or an ADR for decisions that change a project-wide
rule.  An appeal does not override a security embargo, a contributor's privacy,
or the emergency authority above.

If the BDFL is unavailable, the appeal remains pending; contributors must not
manufacture a governing vote or transfer authority by silence.  A successor
recorded under <<continuity>> may resolve pending appeals within the authority
explicitly granted to them.

[[lg-103-confirmation]]
== LG-103 External Confirmation Checklist

The following evidence is required before the project may state that
maintainer-continuity coverage is complete or mark LG-103 done:

* a real person has explicitly consented, in a durable project record, to be a
  backup security responder;
* that record gives a private reporting route, the responder's acknowledgement
  duty and availability expectations, and authority to triage or escalate a
  report when the BDFL is unavailable;
* a real independent maintainer has explicitly consented to review protected
  changes, with their permitted surfaces and any time limit recorded here; and
* the BDFL and the consenting people have reviewed the emergency procedure and
  confirmed that the recorded contact routes work.

Until all four confirmations exist, this repository has only the preparation
described in this document: it does not have a backup security responder or
independent protected-change review coverage.

[[maintainer-records]]
== Maintainer Records

Durable governance and compatibility decisions belong in the repository's ADRs
or other versioned project records. Pull requests and issues should link to the
relevant record so future maintainers can understand both the decision and its
scope.
