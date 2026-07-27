# Kotlin database fixture

`database.sqlite` and `tasks.sqlite` were generated from Komga commit
`4ef86df96e183612de610dc08a26f851d1e66458` with Gradle 8.14.3 and Java 21:

```bash
cd komga
gradle :komga:flywayMigrateMain :komga:flywayMigrateTasks
```

The generated databases were copied from
`komga/komga/build/generated/flyway/{main,tasks}`. Representative library,
exclusion, client-setting, and scan-task rows were then inserted with SQLite.
The rows use the field values and task serialization contract from the same
Kotlin source revision.

Tests copy these files before opening them because the Rust compatibility
migrations are additive and must never modify the checked-in fixtures.

The schema contract pins the 51 supported main-database tables and the task
table. A fresh Rust database and a migrated fixture must preserve compatible
SQLite affinities and primary keys for every baseline column, every named index,
and every foreign-key relationship. Separate round-trip coverage exercises all
supported persistence domains against a copied fixture and verifies that both
Flyway and SQLx histories survive repeated opening. Historical Flyway upgrade
paths before this generated baseline are intentionally not part of the fixture.
