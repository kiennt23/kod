# GitHub webhook fixtures

Projections of the public GitHub.com payload examples maintained by Octokit,
pinned to commit `aa80c24a3f55354465b8b1438d33dd5602956d0d`:

- [issues/labeled](https://github.com/octokit/webhooks/blob/aa80c24a3f55354465b8b1438d33dd5602956d0d/payload-examples/api.github.com/issues/labeled.payload.json)
- [issues/unlabeled](https://github.com/octokit/webhooks/blob/aa80c24a3f55354465b8b1438d33dd5602956d0d/payload-examples/api.github.com/issues/unlabeled.payload.json)
- [issues/edited](https://github.com/octokit/webhooks/blob/aa80c24a3f55354465b8b1438d33dd5602956d0d/payload-examples/api.github.com/issues/edited.payload.json)
- [issue_comment/created](https://github.com/octokit/webhooks/blob/aa80c24a3f55354465b8b1438d33dd5602956d0d/payload-examples/api.github.com/issue_comment/created.payload.json)

Only fields consumed by this adapter are retained: action, repository name,
sender identity, issue identity/state/body/labels, label name and comment
identity/body. Values are unchanged. Signatures are generated locally with
the test secret; no production signature or secret is stored.

The shared contract uses these payloads adapted to the local HTTP sandbox's
repository, actor and trigger label. A separate test replays their original
repository and actor identities. This is offline fixture acceptance, not a
live webhook delivery or deployment test. The upstream MIT license is
included alongside these files.
