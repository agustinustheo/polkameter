# JUnit schema

`junit.xsd` is the unmodified Jenkins JUnit schema from JUnit's own legacy XML report tests:

https://github.com/junit-team/junit-framework/blob/95b24d54861752d797510da89071c549c26355c2/platform-tests/src/test/resources/jenkins-junit.xsd

It carries the MIT licence and the Thales Corporate Services SAS copyright notice in its header. The CLI JUnit integration test validates a load report and a failed run report against it with `xmllint --schema`.
