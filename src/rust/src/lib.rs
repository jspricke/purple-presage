//#![no_std]
#![no_main]

use futures::{channel::oneshot, future};
use presage::{prelude::SignalServers, Manager};
use presage_store_sled::{MigrationConflictStrategy, SledStore};

#[repr(C)]
pub struct Presage {
    pub account: *const std::os::raw::c_void,
    //pub tx_ptr: *mut tokio::sync::mpsc::Sender<Cmd>,
    pub tx_ptr: *mut std::os::raw::c_void,
    pub qrcode: *const std::os::raw::c_char,
    pub uuid: *const std::os::raw::c_char,
}

extern "C" {
    fn presage_append_message(input: *const Presage);
}

// https://stackoverflow.com/questions/66196972/how-to-pass-a-reference-pointer-to-a-rust-struct-to-a-c-ffi-interface
#[no_mangle]
pub extern "C" fn presage_rust_init() -> *mut tokio::runtime::Runtime {
    // https://stackoverflow.com/questions/64658556/
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_name("presage Tokio")
        .enable_io()
        .enable_time()
        .build()
        .unwrap();
    let runtime_box = Box::new(runtime);
    Box::into_raw(runtime_box)
}

#[no_mangle]
pub extern "C" fn presage_rust_destroy(runtime: *mut tokio::runtime::Runtime) {
    unsafe {
        drop(Box::from_raw(runtime));
    }
}

use presage::Store;

// from main
pub enum Cmd {
    LinkDevice {
        servers: SignalServers,
        device_name: String,
    },
    Whoami,
}

async fn run<C: Store + 'static>(
    subcommand: Cmd,
    config_store: C,
    account: *const std::os::raw::c_void,
) {
    match subcommand {
        Cmd::LinkDevice {
            servers,
            device_name,
        } => {
            let (provisioning_link_tx, provisioning_link_rx) = oneshot::channel();
            let manager = future::join(
                Manager::link_secondary_device(
                    config_store,
                    servers,
                    device_name.clone(),
                    provisioning_link_tx,
                ),
                async move {
                    match provisioning_link_rx.await {
                        Ok(url) => {
                            println!("rust: qr code ok.");
                            println!("rust: now calling presage_append_message…");
                            let message = Presage {
                                account: account,
                                tx_ptr: std::ptr::null_mut(),
                                qrcode: std::ffi::CString::new(url.to_string()).unwrap().into_raw(),
                                uuid: std::ptr::null(),
                            };
                            unsafe {
                                presage_append_message(&message);
                            }
                        }
                        Err(e) => println!("Error linking device: {e}"),
                    }
                },
            )
            .await;

            match manager {
                (Ok(manager), _) => {
                    let uuid = manager.whoami().await.unwrap().uuid;
                    let message = Presage {
                        account: account,
                        tx_ptr: std::ptr::null_mut(),
                        qrcode: std::ptr::null(),
                        uuid: std::ffi::CString::new(uuid.to_string()).unwrap().into_raw(),
                    };
                    unsafe {
                        presage_append_message(&message);
                    }
                }
                (Err(err), _) => {
                    println!("{err:?}");
                }
            }
        }
        Cmd::Whoami => {
            let mut uuid = String::from("");
            let manager = Manager::load_registered(config_store).await;
            match manager {
                Ok(manager) => {
                    let whoami = manager.whoami().await;
                    match whoami {
                        Ok(whoami) => {
                            uuid = whoami.uuid.to_string();
                        }
                        Err(err) => {
                            // TODO: find out if this one is showing ServiceError(Unauthorized)
                            println!("rust: whoami Err {err:?}");
                        }
                    }
                }
                Err(err) => {
                    println!("rust: whoami manager Err {err:?}");
                }
            }
            let message = Presage {
                account: account,
                tx_ptr: std::ptr::null_mut(),
                qrcode: std::ptr::null(),
                uuid: std::ffi::CString::new(uuid).unwrap().into_raw(),
            };
            unsafe {
                presage_append_message(&message);
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn presage_rust_main(
    rt: *mut tokio::runtime::Runtime,
    account: *const std::os::raw::c_void,
    c_store_path: *const std::os::raw::c_char,
) {
    let store_path: String = std::ffi::CStr::from_ptr(c_store_path)
        .to_str()
        .unwrap()
        .to_owned();
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let tx_ptr = Box::into_raw(Box::new(tx));
    let message = Presage {
        account: account,
        tx_ptr: tx_ptr as *mut std::os::raw::c_void,
        qrcode: std::ptr::null(),
        uuid: std::ptr::null(),
    };
    unsafe {
        presage_append_message(&message);
    }
    let runtime = rt.as_ref().unwrap();
    runtime.block_on(async {
        // from main
        let passphrase: Option<String> = None;
        //println!("rust: opening config database from {store_path}");
        let config_store = SledStore::open_with_passphrase(
            store_path,
            passphrase,
            MigrationConflictStrategy::Raise,
        );
        match config_store {
            Ok(config_store) => {
                println!("rust: config_store OK");
                while let Some(cmd) = rx.recv().await {
                    // TODO: find out if config_store.clone() is the correct thing to do here
                    run(cmd, config_store.clone(), account).await
                }
            }
            Err(err) => {
                println!("rust: config_store Err {err:?}");
            }
        }
    });
}

// let mut manager = Manager::load_registered(config_store).await?;

unsafe fn send_cmd(
    rt: *mut tokio::runtime::Runtime,
    tx: *mut tokio::sync::mpsc::Sender<Cmd>,
    cmd: Cmd,
) {
    let command_tx = tx.as_ref().unwrap();
    let runtime = rt.as_ref().unwrap();
    match runtime.block_on(command_tx.send(cmd)) {
        Ok(()) => {
            println!("rust: command_tx.send OK");
        }
        Err(err) => {
            println!("rust: command_tx.send {err}");
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn presage_rust_link(
    rt: *mut tokio::runtime::Runtime,
    tx: *mut tokio::sync::mpsc::Sender<Cmd>,
    c_device_name: *const std::os::raw::c_char,
) {
    let device_name: String = std::ffi::CStr::from_ptr(c_device_name)
        .to_str()
        .unwrap()
        .to_owned();
    println!("rust: presage_rust_link invoked successfully! device_name is {device_name}");
    // from args
    let server: SignalServers = SignalServers::Production;
    //let server: SignalServers = SignalServers::Staging;
    let cmd: Cmd = Cmd::LinkDevice {
        device_name: device_name,
        servers: server,
    };
    send_cmd(rt, tx, cmd);
    println!("rust: presage_rust_link ends now");
}

#[no_mangle]
pub unsafe extern "C" fn presage_rust_whoami(
    rt: *mut tokio::runtime::Runtime,
    tx: *mut tokio::sync::mpsc::Sender<Cmd>,
) {
    let cmd: Cmd = Cmd::Whoami {};
    send_cmd(rt, tx, cmd);
}
