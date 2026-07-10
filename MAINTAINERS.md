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

Changes to the following protected surfaces require review by a maintainer who
did not author the change before merge:

* public CLI, MCP, graph, store, or result-schema compatibility;
* graph/store migrations and their rollback paths;
* authentication, secret handling, path confinement, command execution, or
  vulnerability remediation; and
* governance, licensing, contributor, or release-policy documents.

When no independent maintainer is available, the change remains open for normal
review. It must not self-declare independent approval or bypass the requirement.
The BDFL may make an explicit, documented exception only to contain an active
security incident or prevent immediate data loss; the rationale, scope, and
follow-up review must be recorded in the pull request or issue.

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

[[maintainer-records]]
== Maintainer Records

Durable governance and compatibility decisions belong in the repository's ADRs
or other versioned project records. Pull requests and issues should link to the
relevant record so future maintainers can understand both the decision and its
scope.
