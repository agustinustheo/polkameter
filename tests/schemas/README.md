# JUnit schema

`junit.xsd` is the unmodified Jenkins JUnit schema used by JUnit's own legacy XML
report tests, copied from:

https://github.com/junit-team/junit-framework/blob/95b24d54861752d797510da89071c549c26355c2/platform-tests/src/test/resources/jenkins-junit.xsd

The schema carries the MIT licence and Thales Corporate Services SAS copyright
notice in its header. It describes the common Jenkins-style JUnit dialect,
including testcase output, also consumed by GitLab and GitHub test-report actions.
There is no single official JUnit XML schema shared by every CI product.

The CLI JUnit integration test validates both the recorded load report and a failed
workflow report with `xmllint --schema`. Locally it skips validation only when
`xmllint` is unavailable; in CI a missing executable fails the test. CI installs
`libxml2-utils`.
