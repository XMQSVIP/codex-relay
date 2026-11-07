use super::*;
use crate::operations::StatsQuery;

#[test]
fn independent_quota_refresh_preserves_running_inference() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let account = Uuid::new_v4();
    ops.register_accounts(&[(account, "busy account".into())])
        .unwrap();
    let inference = ops
        .begin(Some(account), None, "inference", "responses", true)
        .unwrap();
    for outcome in ["success", "failure", "cancelled"] {
        let refresh = ops
            .begin(Some(account), None, "management", "quota_refresh", false)
            .unwrap();
        assert_eq!(ops.snapshot()["active"], 1);
        assert_eq!(ops.clear_request_history().unwrap_err().code, "BUSY");
        match outcome {
            "success" => refresh.complete(Ok(())).unwrap(),
            "failure" => assert_eq!(
                refresh
                    .complete::<()>(Err(Error::new("RPC_TIMEOUT", "timeout")))
                    .unwrap_err()
                    .code,
                "RPC_TIMEOUT"
            ),
            _ => drop(refresh),
        }
        assert_eq!(ops.snapshot()["active"], 1);
    }
    inference.complete(Ok(())).unwrap();
    assert_eq!(ops.snapshot()["active"], 0);
    assert_eq!(
        ops.statistics(StatsQuery::default()).unwrap()["summary"]["total"],
        1
    );
}

#[test]
fn subscription_reads_share_busy_accounts_and_release_only_their_own_lease() {
    for outcome in ["success", "failure", "cancelled"] {
        let temp = tempfile::tempdir().unwrap();
        let ops = Operations::open(temp.path()).unwrap();
        let account = Uuid::new_v4();
        ops.register_accounts(&[(account, "busy subscription account".into())])
            .unwrap();
        ops.inner
            .lock()
            .unwrap()
            .settings
            .accounts
            .iter_mut()
            .find(|a| a.id == account)
            .unwrap()
            .max_concurrency = 1;
        let inference = ops
            .begin(Some(account), None, "inference", "responses", true)
            .unwrap();
        let refresh = ops
            .begin(
                Some(account),
                None,
                "management",
                "subscription_refresh",
                false,
            )
            .unwrap();
        assert_eq!(ops.snapshot()["active"], 1);
        assert_eq!(ops.inner.lock().unwrap().active.len(), 2);

        inference.complete(Ok(())).unwrap();
        // A new inference can start while the independent query is still running.
        let next = ops
            .begin(Some(account), None, "inference", "responses", true)
            .unwrap();
        match outcome {
            "success" => refresh.complete(Ok(())).unwrap(),
            "failure" => assert_eq!(
                refresh
                    .complete::<()>(Err(Error::new("RPC_TIMEOUT", "timeout")))
                    .unwrap_err()
                    .code,
                "RPC_TIMEOUT"
            ),
            _ => drop(refresh),
        }
        assert_eq!(ops.snapshot()["active"], 1);
        assert_eq!(ops.inner.lock().unwrap().active.len(), 1);
        assert_eq!(ops.clear_request_history().unwrap_err().code, "BUSY");
        next.complete(Ok(())).unwrap();
        assert!(ops.inner.lock().unwrap().active.is_empty());
        assert_eq!(ops.snapshot()["storageHealthy"], true);

        ops.start_shutdown().unwrap();
        assert_eq!(
            ops.begin(
                Some(account),
                None,
                "management",
                "subscription_refresh",
                false
            )
            .err()
            .unwrap()
            .code,
            "SERVICE_DRAINING"
        );
    }
}

#[test]
fn quota_refresh_reserves_capacity_without_request_history() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let account = Uuid::new_v4();
    ops.register_accounts(&[(account, "account".into())])
        .unwrap();

    for outcome in ["success", "failure", "cancelled"] {
        let lease = ops
            .begin_account_maintenance(account, "management", "quota_refresh")
            .unwrap();
        assert_eq!(ops.snapshot()["active"], 1);
        assert_eq!(
            ops.statistics(StatsQuery::default()).unwrap()["summary"]["total"],
            0
        );
        assert_eq!(ops.clear_request_history().unwrap_err().code, "BUSY");
        assert_eq!(
            ops.begin_account_maintenance(account, "management", "quota_refresh")
                .err()
                .unwrap()
                .code,
            "CONCURRENCY_LIMIT"
        );
        match outcome {
            "success" => lease.complete(Ok(())).unwrap(),
            "failure" => assert_eq!(
                lease
                    .complete::<()>(Err(Error::new("QUOTA_UNSUPPORTED", "unsupported")))
                    .unwrap_err()
                    .code,
                "QUOTA_UNSUPPORTED"
            ),
            _ => drop(lease),
        }
        assert_eq!(ops.snapshot()["active"], 0);
        assert_eq!(ops.snapshot()["storageHealthy"], true);
        assert_eq!(ops.clear_request_history().unwrap(), 0);
    }

    for (kind, action) in [
        ("management", "quota_reset"),
        ("diagnostic", "connection_test"),
        ("inference", "responses"),
    ] {
        ops.begin(Some(account), None, kind, action, true)
            .unwrap()
            .complete(Ok(()))
            .unwrap();
    }
    assert_eq!(
        ops.statistics(StatsQuery::default()).unwrap()["summary"]["total"],
        3
    );
}

#[test]
fn reopening_preserves_current_history_and_recovers_interrupted_work() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    for (id, status) in [
        ("success", "succeeded"),
        ("failed", "failed"),
        ("running", "running"),
    ] {
        ops.inner.lock().unwrap().db.execute("INSERT INTO requests(id,started_at,kind,action,status) VALUES(?1,?2,'inference','responses',?3)",params![id,crate::now_ms() as i64,status]).unwrap();
    }
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    let stats = ops.statistics(StatsQuery::default()).unwrap();
    assert_eq!(stats["summary"]["total"], 3);
    assert!(
        stats["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "running" && r["status"] == "interrupted")
    );
    assert_eq!(ops.clear_request_history().unwrap(), 3);
}
