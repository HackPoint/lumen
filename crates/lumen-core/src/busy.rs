//! Waiting for a ledger that another connection is using for a moment.
//!
//! SQLite answers SQLITE_BUSY at once, without consulting the busy timeout, when a connection
//! that already holds a read lock asks for the write lock while another connection holds it:
//! waiting could deadlock the two. Switching a ledger to WAL does exactly that, and every ledger
//! starts in the rollback-journal mode, so two programs that open one in the same instant (the
//! daemon and a hook, the app and the daemon) can hear "database is locked" from an open that
//! a short wait would have let through. The loser has dropped its lock by the time it hears of
//! the failure, and the winner finishes the switch for everyone.
//!
//! Openers retry for [`BUSY_BUDGET`] and then give the error back.

use std::future::Future;
use std::time::Duration;

/// How long an opener keeps trying a busy ledger, counted from its first attempt.
pub const BUSY_BUDGET: Duration = Duration::from_secs(2);

/// Whether `e` is SQLITE_BUSY in any of its extended forms.
pub fn is_busy(_e: &sqlx::Error) -> bool {
    false
}

/// Whether `e` is SQLITE_BUSY in any of its extended forms.
pub fn is_busy_rusqlite(_e: &rusqlite::Error) -> bool {
    false
}

/// Run `op`, again while it fails with SQLITE_BUSY, for up to [`BUSY_BUDGET`].
pub async fn retry_busy<T, F, Fut>(op: F) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, sqlx::Error>>,
{
    retry_busy_for(BUSY_BUDGET, op).await
}

async fn retry_busy_for<T, F, Fut>(_budget: Duration, mut op: F) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, sqlx::Error>>,
{
    op().await
}

/// [`retry_busy`] for the rusqlite openers, which block.
pub fn retry_busy_blocking<T>(op: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    retry_busy_blocking_for(BUSY_BUDGET, op)
}

fn retry_busy_blocking_for<T>(
    _budget: Duration,
    mut op: impl FnMut() -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    op()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::cell::Cell;
    use std::fmt;
    use std::time::Instant;

    /// A database error that carries only a result code, as sqlx's SQLite driver reports one.
    #[derive(Debug)]
    struct Coded(i32);

    impl fmt::Display for Coded {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "(code: {}) coded", self.0)
        }
    }

    impl std::error::Error for Coded {}

    impl sqlx::error::DatabaseError for Coded {
        fn message(&self) -> &str {
            "coded"
        }
        fn code(&self) -> Option<Cow<'_, str>> {
            Some(self.0.to_string().into())
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    fn sqlx_code(code: i32) -> sqlx::Error {
        sqlx::Error::Database(Box::new(Coded(code)))
    }

    fn rusqlite_code(code: i32) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
    }

    #[test]
    fn sqlx_busy_is_every_extended_form_of_code_5_and_nothing_else() {
        // 5 BUSY, 261 BUSY_RECOVERY, 517 BUSY_SNAPSHOT, 773 BUSY_TIMEOUT.
        for code in [5, 261, 517, 773] {
            assert!(is_busy(&sqlx_code(code)), "code {code} is SQLITE_BUSY");
        }
        // 1 ERROR, 6 LOCKED (a table held inside one process, which waiting does not cure),
        // 19 CONSTRAINT, 1555 CONSTRAINT_PRIMARYKEY.
        for code in [1, 6, 19, 1555] {
            assert!(!is_busy(&sqlx_code(code)), "code {code} is not SQLITE_BUSY");
        }
        assert!(!is_busy(&sqlx::Error::PoolTimedOut));
        assert!(!is_busy(&sqlx::Error::RowNotFound));
    }

    #[test]
    fn rusqlite_busy_is_every_extended_form_of_code_5_and_nothing_else() {
        for code in [5, 261, 517, 773] {
            assert!(
                is_busy_rusqlite(&rusqlite_code(code)),
                "code {code} is SQLITE_BUSY"
            );
        }
        for code in [1, 6, 19, 1555] {
            assert!(
                !is_busy_rusqlite(&rusqlite_code(code)),
                "code {code} is not SQLITE_BUSY"
            );
        }
        assert!(!is_busy_rusqlite(&rusqlite::Error::QueryReturnedNoRows));
    }

    #[test]
    fn blocking_retry_runs_again_until_the_call_works() {
        let calls = Cell::new(0);
        let out = retry_busy_blocking(|| {
            calls.set(calls.get() + 1);
            if calls.get() < 4 {
                Err(rusqlite_code(5))
            } else {
                Ok(calls.get())
            }
        });
        assert_eq!(out.unwrap(), 4);
        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn blocking_retry_does_not_repeat_an_error_that_is_not_busy() {
        let calls = Cell::new(0);
        let out: rusqlite::Result<()> = retry_busy_blocking(|| {
            calls.set(calls.get() + 1);
            Err(rusqlite_code(1))
        });
        assert!(out.is_err());
        assert_eq!(calls.get(), 1, "a duplicate column is not a reason to wait");
    }

    #[test]
    fn blocking_retry_gives_the_busy_error_back_when_the_budget_is_spent() {
        let calls = Cell::new(0);
        let started = Instant::now();
        let out: rusqlite::Result<()> =
            retry_busy_blocking_for(Duration::from_millis(120), || {
                calls.set(calls.get() + 1);
                Err(rusqlite_code(5))
            });
        let took = started.elapsed();
        assert!(is_busy_rusqlite(&out.unwrap_err()));
        assert!(calls.get() >= 3, "tried {} times in 120 ms", calls.get());
        assert!(took >= Duration::from_millis(120), "gave up after {took:?}");
        assert!(took < Duration::from_secs(5), "kept going for {took:?}");
    }

    #[test]
    fn blocking_retry_does_not_repeat_a_call_that_used_the_budget_by_itself() {
        // A call that waited the whole busy timeout and still failed was already given its chance.
        let calls = Cell::new(0);
        let out: rusqlite::Result<()> =
            retry_busy_blocking_for(Duration::from_millis(30), || {
                calls.set(calls.get() + 1);
                std::thread::sleep(Duration::from_millis(60));
                Err(rusqlite_code(5))
            });
        assert!(is_busy_rusqlite(&out.unwrap_err()));
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn async_retry_runs_again_until_the_call_works() {
        let calls = Cell::new(0);
        let out = retry_busy(|| {
            calls.set(calls.get() + 1);
            let n = calls.get();
            async move { if n < 4 { Err(sqlx_code(5)) } else { Ok(n) } }
        })
        .await;
        assert_eq!(out.unwrap(), 4);
        assert_eq!(calls.get(), 4);
    }

    #[tokio::test]
    async fn async_retry_does_not_repeat_an_error_that_is_not_busy() {
        let calls = Cell::new(0);
        let out: Result<(), sqlx::Error> = retry_busy(|| {
            calls.set(calls.get() + 1);
            async { Err(sqlx_code(1)) }
        })
        .await;
        assert!(out.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn async_retry_gives_the_busy_error_back_when_the_budget_is_spent() {
        let calls = Cell::new(0);
        let started = Instant::now();
        let out: Result<(), sqlx::Error> = retry_busy_for(Duration::from_millis(120), || {
            calls.set(calls.get() + 1);
            async { Err(sqlx_code(5)) }
        })
        .await;
        let took = started.elapsed();
        assert!(is_busy(&out.unwrap_err()));
        assert!(calls.get() >= 3, "tried {} times in 120 ms", calls.get());
        assert!(took >= Duration::from_millis(120), "gave up after {took:?}");
        assert!(took < Duration::from_secs(5), "kept going for {took:?}");
    }
}
