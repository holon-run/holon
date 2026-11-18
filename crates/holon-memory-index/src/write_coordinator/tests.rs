use std::{sync::mpsc, thread, time::Duration};

use tempfile::tempdir;

use super::*;

#[test]
fn coordinator_is_shared_by_normalized_database_path() -> Result<()> {
    let directory = tempdir()?;
    let path = directory.path().join("memory.v2.sqlite3");
    let equivalent = directory.path().join(".").join("memory.v2.sqlite3");

    let first = memory_index_write_coordinator(&path)?;
    let second = memory_index_write_coordinator(&equivalent)?;

    assert!(Arc::ptr_eq(&first, &second));
    Ok(())
}

#[test]
fn coordinator_serves_waiters_in_ticket_order() -> Result<()> {
    let directory = tempdir()?;
    let coordinator = memory_index_write_coordinator(&directory.path().join("memory.v2.sqlite3"))?;
    let first_turn = coordinator.wait_turn_until(
        MemoryIndexWriteClass::Foreground,
        "test.first",
        Instant::now() + Duration::from_secs(5),
    )?;
    let (sender, receiver) = mpsc::channel();
    let mut handles = Vec::new();

    for value in [1, 2] {
        let waiter = Arc::clone(&coordinator);
        let sender = sender.clone();
        handles.push(thread::spawn(move || -> Result<()> {
            let _turn = waiter.wait_turn_until(
                MemoryIndexWriteClass::Foreground,
                "test.waiter",
                Instant::now() + Duration::from_secs(5),
            )?;
            sender.send(value)?;
            Ok(())
        }));
        let expected_next_ticket = u64::try_from(value + 1)?;
        for _ in 0..100 {
            if coordinator
                .state
                .lock()
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                .next_ticket
                >= expected_next_ticket
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            coordinator
                .state
                .lock()
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                .next_ticket,
            expected_next_ticket
        );
    }
    drop(first_turn);

    assert_eq!(receiver.recv_timeout(Duration::from_secs(1))?, 1);
    assert_eq!(receiver.recv_timeout(Duration::from_secs(1))?, 2);
    for handle in handles {
        handle.join().expect("writer waiter thread panicked")?;
    }
    Ok(())
}

#[test]
fn coordinator_prioritizes_foreground_then_resumes_maintenance_fifo() -> Result<()> {
    let directory = tempdir()?;
    let coordinator = memory_index_write_coordinator(&directory.path().join("memory.v2.sqlite3"))?;
    let active_turn = coordinator.wait_turn_until(
        MemoryIndexWriteClass::Foreground,
        "test.active",
        Instant::now() + Duration::from_secs(5),
    )?;
    let (sender, receiver) = mpsc::channel();
    let mut handles = Vec::new();

    for value in [1, 2] {
        let waiter = Arc::clone(&coordinator);
        let sender = sender.clone();
        handles.push(thread::spawn(move || -> Result<()> {
            let _turn = waiter.wait_turn_until(
                MemoryIndexWriteClass::Maintenance,
                "test.maintenance",
                Instant::now() + Duration::from_secs(5),
            )?;
            sender.send(value)?;
            Ok(())
        }));
        let expected_depth = usize::try_from(value)?;
        for _ in 0..100 {
            if coordinator
                .state
                .lock()
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                .maintenance_waiters
                .len()
                == expected_depth
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            coordinator
                .state
                .lock()
                .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
                .maintenance_waiters
                .len(),
            expected_depth
        );
    }

    let foreground_waiter = Arc::clone(&coordinator);
    let foreground_sender = sender.clone();
    handles.push(thread::spawn(move || -> Result<()> {
        let _turn = foreground_waiter.wait_turn_until(
            MemoryIndexWriteClass::Foreground,
            "test.foreground",
            Instant::now() + Duration::from_secs(5),
        )?;
        foreground_sender.send(0)?;
        Ok(())
    }));
    for _ in 0..100 {
        if coordinator
            .state
            .lock()
            .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
            .foreground_waiters
            .len()
            == 1
        {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        coordinator
            .state
            .lock()
            .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
            .queue_depths(),
        QueueDepths {
            total: 3,
            foreground: 1,
            maintenance: 2,
        }
    );

    drop(active_turn);
    assert_eq!(receiver.recv_timeout(Duration::from_secs(1))?, 0);
    assert_eq!(receiver.recv_timeout(Duration::from_secs(1))?, 1);
    assert_eq!(receiver.recv_timeout(Duration::from_secs(1))?, 2);
    for handle in handles {
        handle.join().expect("writer waiter thread panicked")?;
    }
    let state = coordinator
        .state
        .lock()
        .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?;
    assert_eq!(state.active_ticket, None);
    assert_eq!(state.queue_depths(), QueueDepths::default());
    Ok(())
}

#[test]
fn coordinator_releases_turn_when_writer_panics() -> Result<()> {
    let directory = tempdir()?;
    let coordinator = memory_index_write_coordinator(&directory.path().join("memory.v2.sqlite3"))?;
    let first_turn = coordinator.wait_turn_until(
        MemoryIndexWriteClass::Foreground,
        "test.first",
        Instant::now() + Duration::from_secs(5),
    )?;

    let panicking_coordinator = Arc::clone(&coordinator);
    let panicking_handle = thread::spawn(move || {
        let _turn = panicking_coordinator
            .wait_turn_until(
                MemoryIndexWriteClass::Foreground,
                "test.panicking",
                Instant::now() + Duration::from_secs(5),
            )
            .expect("panicking writer failed to acquire turn");
        panic!("test writer panic");
    });
    for _ in 0..100 {
        if coordinator
            .state
            .lock()
            .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
            .next_ticket
            >= 2
        {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        coordinator
            .state
            .lock()
            .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
            .next_ticket,
        2
    );

    let successor_coordinator = Arc::clone(&coordinator);
    let (sender, receiver) = mpsc::channel();
    let successor_handle = thread::spawn(move || -> Result<()> {
        let _turn = successor_coordinator.wait_turn_until(
            MemoryIndexWriteClass::Foreground,
            "test.successor",
            Instant::now() + Duration::from_secs(5),
        )?;
        sender.send(())?;
        Ok(())
    });
    for _ in 0..100 {
        if coordinator
            .state
            .lock()
            .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
            .next_ticket
            >= 3
        {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        coordinator
            .state
            .lock()
            .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
            .next_ticket,
        3
    );

    drop(first_turn);
    assert!(panicking_handle.join().is_err());
    receiver.recv_timeout(Duration::from_secs(1))?;
    successor_handle
        .join()
        .expect("successor writer thread panicked")?;
    Ok(())
}

#[test]
fn coordinator_timeout_cancels_ticket_and_releases_successor() -> Result<()> {
    let directory = tempdir()?;
    let coordinator = memory_index_write_coordinator(&directory.path().join("memory.v2.sqlite3"))?;
    let first_turn = coordinator.wait_turn_until(
        MemoryIndexWriteClass::Foreground,
        "test.first",
        Instant::now() + Duration::from_secs(5),
    )?;

    let timed_out_coordinator = Arc::clone(&coordinator);
    let timed_out_handle = thread::spawn(move || {
        match timed_out_coordinator.wait_turn_until(
            MemoryIndexWriteClass::Foreground,
            "test.timeout",
            Instant::now() + Duration::from_millis(50),
        ) {
            Ok(_turn) => panic!("queued writer should time out"),
            Err(error) => error,
        }
    });
    for _ in 0..100 {
        if coordinator
            .state
            .lock()
            .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
            .next_ticket
            >= 2
        {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }

    let successor_coordinator = Arc::clone(&coordinator);
    let (sender, receiver) = mpsc::channel();
    let successor_handle = thread::spawn(move || -> Result<()> {
        let _turn = successor_coordinator.wait_turn_until(
            MemoryIndexWriteClass::Foreground,
            "test.successor",
            Instant::now() + Duration::from_secs(5),
        )?;
        sender.send(())?;
        Ok(())
    });
    for _ in 0..100 {
        if coordinator
            .state
            .lock()
            .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
            .next_ticket
            >= 3
        {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }

    let timeout_error = timed_out_handle
        .join()
        .expect("timed out writer thread panicked");
    assert!(timeout_error.to_string().contains("timed out"));
    assert!(
        receiver.recv_timeout(Duration::from_millis(100)).is_err(),
        "successor writer acquired while the first turn was still active"
    );
    drop(first_turn);

    receiver.recv_timeout(Duration::from_secs(1))?;
    successor_handle
        .join()
        .expect("successor writer thread panicked")?;
    Ok(())
}

#[test]
fn maintenance_timeout_does_not_block_foreground_successor() -> Result<()> {
    let directory = tempdir()?;
    let coordinator = memory_index_write_coordinator(&directory.path().join("memory.v2.sqlite3"))?;
    let active_turn = coordinator.wait_turn_until(
        MemoryIndexWriteClass::Foreground,
        "test.active",
        Instant::now() + Duration::from_secs(5),
    )?;

    let timed_out_coordinator = Arc::clone(&coordinator);
    let timed_out_handle = thread::spawn(move || {
        match timed_out_coordinator.wait_turn_until(
            MemoryIndexWriteClass::Maintenance,
            "test.maintenance_timeout",
            Instant::now() + Duration::from_millis(50),
        ) {
            Ok(_turn) => panic!("queued maintenance writer should time out"),
            Err(error) => error,
        }
    });
    for _ in 0..100 {
        if coordinator
            .state
            .lock()
            .map_err(|_| anyhow!("memory index write coordinator mutex poisoned"))?
            .maintenance_waiters
            .len()
            == 1
        {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }

    let successor_coordinator = Arc::clone(&coordinator);
    let (sender, receiver) = mpsc::channel();
    let successor_handle = thread::spawn(move || -> Result<()> {
        let _turn = successor_coordinator.wait_turn_until(
            MemoryIndexWriteClass::Foreground,
            "test.foreground_successor",
            Instant::now() + Duration::from_secs(5),
        )?;
        sender.send(())?;
        Ok(())
    });

    let timeout_error = timed_out_handle
        .join()
        .expect("timed out maintenance writer thread panicked");
    assert!(timeout_error.to_string().contains("timed out"));
    assert!(
        receiver.recv_timeout(Duration::from_millis(100)).is_err(),
        "foreground successor acquired while the active turn was still held"
    );
    drop(active_turn);

    receiver.recv_timeout(Duration::from_secs(1))?;
    successor_handle
        .join()
        .expect("foreground successor thread panicked")?;
    Ok(())
}
