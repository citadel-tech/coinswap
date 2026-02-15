#![cfg(feature = "integration-test")]

use bitcoin::{Amount, OutPoint};
use coinswap::{
    maker::{
        start_maker_server, start_maker_server_taproot, Maker, MakerBehavior, RpcMsgReq,
        RpcMsgResp, TaprootMaker, TaprootMakerBehavior,
    },
    taker::{api2::TakerBehavior as TaprootTakerBehavior, SwapParams, TakerBehavior},
    utill::{read_message, send_message},
};
use std::{
    fs,
    net::TcpStream,
    path::PathBuf,
    str::FromStr,
    sync::{atomic::Ordering::Relaxed, Arc, Mutex, OnceLock},
    thread,
    time::Duration,
};

mod test_framework;
use test_framework::*;

const MAKER_RPC_BASE_PORT: u16 = 3500;

fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn maker_rpc_request(port: u16, req: RpcMsgReq) -> RpcMsgResp {
    let mut stream =
        TcpStream::connect(format!("127.0.0.1:{port}")).expect("maker rpc should be reachable");
    send_message(&mut stream, &req).expect("rpc request should be sent");
    let response_bytes = read_message(&mut stream).expect("rpc response bytes should be readable");
    serde_cbor::from_slice(&response_bytes).expect("rpc response should deserialize")
}

fn add_denied_outpoint_to_all_makers(outpoint: OutPoint, maker_count: usize) {
    for idx in 0..maker_count {
        let rpc_port = MAKER_RPC_BASE_PORT + idx as u16 + 1;
        let response = maker_rpc_request(
            rpc_port,
            RpcMsgReq::AddDeniedUtxo {
                outpoint: outpoint.to_string(),
            },
        );
        match response {
            RpcMsgResp::Text(message) => {
                assert!(
                    message.contains("added") || message.contains("already present"),
                    "unexpected add-deny response: {}",
                    message
                );
            }
            RpcMsgResp::ServerError(err) => {
                panic!("maker rpc error while adding deny outpoint: {}", err)
            }
            other => panic!("unexpected rpc response: {:?}", other),
        }
    }
}

fn spawn_v1_makers(makers: &[Arc<Maker>]) -> Vec<thread::JoinHandle<()>> {
    makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server(maker_clone).expect("maker server should start");
            })
        })
        .collect()
}

fn spawn_v2_makers(makers: &[Arc<TaprootMaker>]) -> Vec<thread::JoinHandle<()>> {
    makers
        .iter()
        .map(|maker| {
            let maker_clone = maker.clone();
            thread::spawn(move || {
                start_maker_server_taproot(maker_clone).expect("taproot maker server should start");
            })
        })
        .collect()
}

fn wait_for_v1_setup(makers: &[Arc<Maker>]) {
    for maker in makers {
        while !maker.is_setup_complete.load(Relaxed) {
            thread::sleep(Duration::from_secs(1));
        }
    }
}

fn wait_for_v2_setup(makers: &[Arc<TaprootMaker>]) {
    for maker in makers {
        while !maker.is_setup_complete.load(Relaxed) {
            thread::sleep(Duration::from_secs(1));
        }
    }
}

fn shutdown_v1_makers(
    makers: &[Arc<Maker>],
    maker_threads: Vec<thread::JoinHandle<()>>,
) {
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().expect("maker thread should join"));
}

fn shutdown_v2_makers(
    makers: &[Arc<TaprootMaker>],
    maker_threads: Vec<thread::JoinHandle<()>>,
) {
    makers
        .iter()
        .for_each(|maker| maker.shutdown.store(true, Relaxed));
    maker_threads
        .into_iter()
        .for_each(|thread| thread.join().expect("taproot maker thread should join"));
}

fn test_outpoint(vout: u32) -> OutPoint {
    OutPoint::new(
        bitcoin::Txid::from_str(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("valid txid"),
        vout,
    )
}

#[test]
fn v1_maker_rejects_denied_taker_outpoint_and_taker_recovers() {
    let _guard = test_lock();
    let makers_config_map = [
        ((6102, Some(19051)), MakerBehavior::Normal),
        ((16102, Some(19052)), MakerBehavior::Normal),
    ];
    let taker_behavior = vec![TakerBehavior::Normal];
    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map.into(), taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taker = &mut takers[0];
    fund_and_verify_taker(taker, bitcoind, 3, Amount::from_btc(0.05).expect("valid amount"));
    fund_and_verify_maker(
        makers.iter().map(Arc::as_ref).collect::<Vec<_>>(),
        bitcoind,
        4,
        Amount::from_btc(0.05).expect("valid amount"),
    );

    taker.get_wallet_mut().sync_and_save().expect("taker sync");
    let denied_outpoint = taker
        .get_wallet()
        .list_all_utxo()
        .first()
        .map(|utxo| OutPoint::new(utxo.txid, utxo.vout))
        .expect("taker should have funded utxos");

    let maker_threads = spawn_v1_makers(&makers);
    wait_for_v1_setup(&makers);
    add_denied_outpoint_to_all_makers(denied_outpoint, makers.len());

    let result = taker.do_coinswap(SwapParams {
        send_amount: Amount::from_sat(100_000),
        maker_count: 2,
        manually_selected_outpoints: Some(vec![denied_outpoint]),
    });
    assert!(
        matches!(result, Ok(None)),
        "expected blocked-policy abort with recovery, got {:?}",
        result
    );

    shutdown_v1_makers(&makers, maker_threads);
    test_framework.stop();
    block_generation_handle
        .join()
        .expect("block generation thread should join");
}

#[test]
fn v1_taker_aborts_on_denied_maker_outpoints_and_recovers() {
    let _guard = test_lock();
    let makers_config_map = [
        ((26102, Some(19053)), MakerBehavior::Normal),
        ((36102, Some(19054)), MakerBehavior::Normal),
    ];
    let taker_behavior = vec![TakerBehavior::Normal];
    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map.into(), taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taker = &mut takers[0];
    fund_and_verify_taker(taker, bitcoind, 3, Amount::from_btc(0.05).expect("valid amount"));
    fund_and_verify_maker(
        makers.iter().map(Arc::as_ref).collect::<Vec<_>>(),
        bitcoind,
        4,
        Amount::from_btc(0.05).expect("valid amount"),
    );

    let maker_threads = spawn_v1_makers(&makers);
    wait_for_v1_setup(&makers);

    // Add deny entries after maker setup so we include post-fidelity and change UTXOs.
    for maker in &makers {
        let mut wallet = maker.get_wallet().write().expect("maker wallet write lock");
        wallet.sync_and_save().expect("maker sync");
        for utxo in wallet.list_all_utxo() {
            taker
                .add_denied_utxo(OutPoint::new(utxo.txid, utxo.vout))
                .expect("denied outpoint should be added");
        }
    }

    let result = taker.do_coinswap(SwapParams {
        send_amount: Amount::from_sat(500_000),
        maker_count: 2,
        manually_selected_outpoints: None,
    });
    assert!(
        matches!(result, Ok(None)),
        "expected blocked-policy abort with recovery, got {:?}",
        result
    );

    shutdown_v1_makers(&makers, maker_threads);
    test_framework.stop();
    block_generation_handle
        .join()
        .expect("block generation thread should join");
}

#[test]
fn v2_maker_rejects_denied_taker_input_and_taker_recovers() {
    let _guard = test_lock();
    let makers_config_map = vec![
        (7102, Some(19061), TaprootMakerBehavior::Normal),
        (17102, Some(19062), TaprootMakerBehavior::Normal),
    ];
    let taker_behavior = vec![TaprootTakerBehavior::Normal];
    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init_taproot(makers_config_map, taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    let taker = takers.get_mut(0).expect("taproot taker should exist");
    fund_taproot_taker(taker, bitcoind, 3, Amount::from_btc(0.05).expect("valid amount"));
    fund_taproot_makers(&makers, bitcoind, 4, Amount::from_btc(0.05).expect("valid amount"));

    taker.get_wallet_mut().sync_and_save().expect("taker sync");
    let denied_outpoint = taker
        .get_wallet()
        .list_all_utxo()
        .first()
        .map(|utxo| OutPoint::new(utxo.txid, utxo.vout))
        .expect("taker should have funded utxos");

    let maker_threads = spawn_v2_makers(&makers);
    wait_for_v2_setup(&makers);
    add_denied_outpoint_to_all_makers(denied_outpoint, makers.len());

    let result = taker.do_coinswap(coinswap::taker::api2::SwapParams {
        send_amount: Amount::from_sat(500_000),
        maker_count: 2,
        tx_count: 1,
        required_confirms: 1,
        manually_selected_outpoints: None,
    });
    assert!(
        matches!(result, Ok(None)),
        "expected blocked-policy abort with recovery, got {:?}",
        result
    );

    shutdown_v2_makers(&makers, maker_threads);
    test_framework.stop();
    block_generation_handle
        .join()
        .expect("block generation thread should join");
}

#[test]
fn deny_list_cli_surface_smoke_via_public_interfaces() {
    let _guard = test_lock();
    let makers_config_map = [((46102, Some(19055)), MakerBehavior::Normal)];
    let taker_behavior = vec![TakerBehavior::Normal];
    let (test_framework, mut takers, makers, block_generation_handle) =
        TestFramework::init(makers_config_map.into(), taker_behavior);

    let bitcoind = &test_framework.bitcoind;
    fund_and_verify_maker(
        makers.iter().map(Arc::as_ref).collect::<Vec<_>>(),
        bitcoind,
        4,
        Amount::from_btc(0.05).expect("valid amount"),
    );

    let maker_threads = spawn_v1_makers(&makers);
    wait_for_v1_setup(&makers);

    // Maker RPC deny-list flow (equivalent to maker-cli commands).
    let rpc_port = MAKER_RPC_BASE_PORT + 1;
    let outpoint = test_outpoint(0).to_string();
    let import_outpoint = test_outpoint(1).to_string();

    let add_resp = maker_rpc_request(
        rpc_port,
        RpcMsgReq::AddDeniedUtxo {
            outpoint: outpoint.clone(),
        },
    );
    assert!(
        matches!(add_resp, RpcMsgResp::Text(_)),
        "unexpected add response: {:?}",
        add_resp
    );

    let list_resp = maker_rpc_request(rpc_port, RpcMsgReq::ListDeniedUtxos);
    match list_resp {
        RpcMsgResp::DeniedUtxosResp { outpoints } => assert!(
            outpoints.contains(&outpoint),
            "added outpoint should be listed in maker deny-list"
        ),
        other => panic!("unexpected list response: {:?}", other),
    }

    let import_file = std::env::temp_dir().join("coinswap-deny-import.txt");
    fs::write(
        &import_file,
        format!("{import_outpoint}\n# comment is ignored\n"),
    )
    .expect("import file should be writable");
    let import_resp = maker_rpc_request(
        rpc_port,
        RpcMsgReq::ImportDeniedUtxos {
            file_path: import_file.to_string_lossy().to_string(),
        },
    );
    assert!(
        matches!(import_resp, RpcMsgResp::Text(_)),
        "unexpected import response: {:?}",
        import_resp
    );

    let clear_resp = maker_rpc_request(rpc_port, RpcMsgReq::ClearDeniedUtxos);
    match clear_resp {
        RpcMsgResp::ClearDeniedUtxosResp { cleared } => {
            assert!(cleared >= 1, "clear should remove at least one entry")
        }
        other => panic!("unexpected clear response: {:?}", other),
    }
    let _ = fs::remove_file(import_file);

    // Taker deny-list flow (equivalent to taker CLI deny-list commands).
    let taker = &mut takers[0];
    let added = taker
        .add_denied_utxo(test_outpoint(2))
        .expect("taker add should succeed");
    assert!(added, "new taker denied outpoint should be added");
    assert_eq!(taker.list_denied_utxos().len(), 1);

    let taker_import_path = PathBuf::from("deny-import.txt");
    let taker_data_dir = std::env::temp_dir()
        .join("coinswap")
        .join("taker1")
        .join(&taker_import_path);
    fs::write(&taker_data_dir, format!("{}\n", test_outpoint(3))).expect("write taker import");
    let imported = taker
        .import_denied_utxos(&taker_import_path)
        .expect("relative import path should resolve under taker data dir");
    assert_eq!(imported, 1, "taker should import one entry");

    let cleared = taker
        .clear_denied_utxos()
        .expect("taker clear should succeed");
    assert!(cleared >= 2, "taker clear should remove existing entries");
    assert!(taker.list_denied_utxos().is_empty());

    shutdown_v1_makers(&makers, maker_threads);
    test_framework.stop();
    block_generation_handle
        .join()
        .expect("block generation thread should join");
}
