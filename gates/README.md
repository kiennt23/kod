# Inactive deployment example

`config.example.json` illustrates the strict harness rules. It is not a
ready-to-activate configuration for this repository. Generated rulesets are
disabled so installing an example cannot block all merges.

The repository currently has one named CODEOWNER. Protected changes need two
distinct non-author reviewers with write access, including a CODEOWNER. The
existing harness policy additionally requires the owner (and backup once
named) to approve. Owner-authored PRs therefore cannot pass this policy, even
if other reviewers are added: the owner's self-approval cannot count.

Before activation, appoint independent reviewers and decide how owner-authored
harness changes are governed. Changing the approval policy requires an owner
decision; this PR does not grant authors self-approval or bypass protection.
Keep the rules disabled until that decision and connected acceptance are done.
