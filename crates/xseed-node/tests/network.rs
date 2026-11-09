//! 多节点集成测试：两个客户端分别连不同的入口节点，完成登记 → 查询 → 跨节点探测。

use std::time::Duration;
use xseed_node::{config::Config, node};
use xseed_transport::client::{Event, NodeClient};
use xseed_transport::registry::Registry;
use xseed_transport::wire::*;
use xseed_transport::{probe, random, PROTO};

struct Net {
    nodes: Vec<node::Shared>,
    reg: Registry,
    _dirs: Vec<tempfile::TempDir>,
}

async fn start(n: usize) -> Net {
    let mut nodes = Vec::new();
    let mut dirs = Vec::new();
    let mut listeners = Vec::new();
    let mut reg = Registry::default();
    for _ in 0..n {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.pow.mailbox_bits = 8;
        cfg.pow.register_bits = 4;
        let node = node::Node::new(dir.path(), cfg, &node::Keys::generate()).unwrap();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut m = node.manifest(&format!("http://{}", l.local_addr().unwrap()));
        m.since = 0;
        reg.nodes.push(m);
        nodes.push(node);
        dirs.push(dir);
        listeners.push(l);
    }
    reg.validate().unwrap();
    for (node, l) in nodes.iter().zip(listeners) {
        node.set_registry(reg.clone());
        tokio::spawn(xseed_node::serve(node.clone(), l, std::future::pending()));
    }
    Net { nodes, reg, _dirs: dirs }
}

fn client(net: &Net, i: usize) -> NodeClient {
    NodeClient::new(net.reg.nodes[i].clone(), xseed_transport::client::http_client())
}

async fn next_env(rx: &mut tokio::sync::mpsc::Receiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("等待消息超时").expect("连接已关闭")
}

#[tokio::test(flavor = "multi_thread")]
async fn register_lookup_probe_across_nodes() {
    let net = start(3).await;
    let sfp: [u8; 32] = random();
    let bucket = 4321u16;

    // A：持有者，入口为节点 0
    let a = client(&net, 0);
    let (wk, _) = a.well_known().await.unwrap();
    let ca = a.create_mailbox(&wk).await.unwrap();
    let mut ma = a.connect(&ca).await.unwrap();
    let holder = a.aliases(&ca, 1).await.unwrap()[0];
    let a_addr = Addr { node: net.reg.nodes[0].tag(), alias: holder };

    // 两跳登记到该桶的全部所有者
    let owners: Vec<_> = net.reg.owners(bucket, xseed_transport::now()).into_iter().cloned().collect();
    assert_eq!(owners.len(), 2);
    for o in &owners {
        let ow = NodeClient::new(o.clone(), xseed_transport::client::http_client());
        let (owk, _) = ow.well_known().await.unwrap();
        let pow = xseed_transport::client::solve_pow(&owk.pow_salt, owk.register_bits).await.unwrap();
        let r = a.ohttp(&ca, o, &GwReq::Register { entries: vec![RegEntry { b: bucket, node: a_addr.node, alias: holder }], ttl_d: 3, pow }).await.unwrap();
        assert_eq!(r, GwResp::Registered { n: 1 });
        // 同一工作量证明不能重复使用
        let again = a.ohttp(&ca, o, &GwReq::Register { entries: vec![RegEntry { b: bucket, node: a_addr.node, alias: holder }], ttl_d: 3, pow }).await;
        assert!(again.unwrap_err().to_string().contains(code::POW_INVALID));
    }

    // B：查询方，入口为节点 1
    let b = client(&net, 1);
    let (wkb, _) = b.well_known().await.unwrap();
    let cb = b.create_mailbox(&wkb).await.unwrap();
    let mut mb = b.connect(&cb).await.unwrap();
    let reply = b.aliases(&cb, 1).await.unwrap()[0];
    let b_addr = Addr { node: net.reg.nodes[1].tag(), alias: reply };

    let GwResp::Found { buckets } = b.ohttp(&cb, &owners[0], &GwReq::Lookup { buckets: vec![bucket, 1] }).await.unwrap() else { panic!() };
    assert_eq!(buckets[0].hits, vec![a_addr]);
    assert!(buckets[1].hits.is_empty());

    // 跨节点探测：节点 1 → 节点 0
    let nonce: [u8; 16] = random();
    let sid: [u8; 16] = random();
    let tag = probe::tag(&sfp, &nonce, &a_addr.alias, &b_addr.alias);
    let env = Envelope { v: PROTO, to: a_addr, from: b_addr, sid, kind: Kind::Probe, body: to_cbor(&ProbeBody { nonce, tag }) };
    mb.tx.send(env).await.unwrap();
    let Event::Env(got) = next_env(&mut ma.rx).await else { panic!() };
    assert_eq!(got.from, b_addr);
    let pb: ProbeBody = from_cbor(&got.body).unwrap();
    assert!(probe::verify(&sfp, &pb.nonce, &got.to.alias, &got.from.alias, &pb.tag));
    // A 回应
    ma.tx.send(Envelope { v: PROTO, to: got.from, from: got.to, sid: got.sid, kind: Kind::ProbeAck, body: vec![] }).await.unwrap();
    let Event::Env(ack) = next_env(&mut mb.rx).await else { panic!() };
    assert_eq!((ack.kind, ack.sid, ack.from), (Kind::ProbeAck, sid, a_addr));

    // 伪造回信地址：用 A 的代号作为 from 从 B 的信箱发出
    let forged = Envelope { v: PROTO, to: a_addr, from: Addr { node: net.reg.nodes[1].tag(), alias: holder }, sid: [9; 16], kind: Kind::Noise, body: vec![] };
    mb.tx.send(forged).await.unwrap();
    let Event::Nack { sid, code } = next_env(&mut mb.rx).await else { panic!() };
    assert_eq!((sid, code.as_str()), ([9; 16], code::ALIAS_INVALID));

    // 目标信箱离线
    drop(ma);
    tokio::time::sleep(Duration::from_millis(300)).await;
    mb.tx.send(Envelope { v: PROTO, to: a_addr, from: b_addr, sid: [7; 16], kind: Kind::Noise, body: vec![] }).await.unwrap();
    let Event::Nack { code, .. } = next_env(&mut mb.rx).await else { panic!() };
    assert_eq!(code, code::MAILBOX_OFFLINE);

    assert!(net.nodes.iter().map(|n| n.index_entries()).sum::<usize>() == 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_bad_auth_and_unsigned_forwarding() {
    let net = start(1).await;
    let c = client(&net, 0);
    let (wk, _) = c.well_known().await.unwrap();
    let mut cred = c.create_mailbox(&wk).await.unwrap();
    cred.secret[0] ^= 1;
    let e = c.aliases(&cred, 1).await.unwrap_err();
    assert!(e.to_string().contains(code::AUTH_FAILED), "{e}");
    assert!(c.connect(&cred).await.is_err());

    let http = reqwest::Client::new();
    let url = &net.reg.nodes[0].url;
    let r = http.post(format!("{url}/v1/fwd")).body(vec![1, 2, 3]).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = http.post(format!("{url}/v1/mailbox")).body(to_cbor(&MailboxReq { pow: Pow { salt: [0; 16], nonce: 0 } })).send().await.unwrap();
    assert_eq!(r.status(), 400, "无效的工作量证明");
    let wk: WellKnown = http.get(format!("{url}/.well-known/xseed?challenge={}", "ab".repeat(32))).send().await.unwrap().json().await.unwrap();
    assert!(wk.challenge_sig.is_some());
}
