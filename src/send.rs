use crate::amount::{Amount, ATOMS_PER_AVI};
use crate::script::p2pkh;
use crate::transaction::{Outpoint, TxOut, Txid};
use crate::utxo::{Coin, UtxoSet};
use crate::wallet::{Wallet, KEY_FILE};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

/// How long the node has to answer. It is a local process serving a handful of
/// JSON; a minute would be politeness to nobody.
const PATIENCE: Duration = Duration::from_secs(10);

/// A body the node could not have meant. `MAX_LISTED` bounds what `/address`
/// returns; this bounds what a wrong port could hand us.
const MAX_ANSWER: usize = 4 * 1024 * 1024;

/// Builds, signs and submits a payment — **on this machine**, with the key
/// never leaving it.
///
/// The API has no `POST /send` and should not: spending authority behind a
/// public URL is the one thing a node must not offer. So the split is that the
/// node holds the chain and this holds the key, and what crosses between them
/// is a signed transaction — the same thing any stranger could have sent.
pub fn send(data_dir: &Path, api: SocketAddr, to: &str, amount: Amount, fee: Amount) -> Result<()> {
    let wallet = Wallet::read(&data_dir.join(KEY_FILE)).with_context(|| {
        format!(
            "reading the key from {}; `send` never mints one, because a key the \
             node has not got is an address nobody will pay",
            data_dir.display()
        )
    })?;

    let status = ask(api, "/status")?;
    let network = crate::params::by_name(text(&status, "network")?)?;
    let height = number(&status, "height")? as u32;

    let address = wallet.address().to_string();
    let held = ask(api, &format!("/address/{address}"))?;
    let utxo = coins_of(&held, &wallet)?;

    // Before anything is selected or signed, and in the terms the operator
    // asked in: "short by 3.5 AVI" is a thing to act on, where "selection
    // failed" is not.
    let held = wallet.balance(&utxo, height + 1, network)?;
    let needed = amount
        .checked_add(fee)
        .context("the amount and the fee sum past MAX_MONEY")?;
    if held < needed {
        bail!(
            "{address} holds {} AVI and this needs {} — short by {} AVI",
            held.in_avi(),
            needed.in_avi(),
            Amount::from_atoms(needed.atoms() - held.atoms())?.in_avi()
        );
    }

    let payment = wallet
        .build(&utxo, height + 1, network)
        .pay(to, amount)?
        .fee(fee)
        .sign()?;

    let raw = payment.get_raw_format();
    let accepted = tell(api, "/tx", &hex::encode(&raw))?;

    println!("{}", text(&accepted, "txid")?);

    Ok(())
}

/// What the node says this address holds, as a set the builder can select
/// from. The script is not in the answer and does not need to be: every coin
/// here pays this wallet, which is the only reason it is here.
fn coins_of(held: &Value, wallet: &Wallet) -> Result<UtxoSet> {
    let script = p2pkh(&wallet.pubkey_hash());
    let unspent = held["unspent"]
        .as_array()
        .context("the node's answer has no unspent list")?;

    let mut coins = Vec::with_capacity(unspent.len());
    for coin in unspent {
        let txid: [u8; 32] = hex::decode(text(coin, "txid")?)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .context("an unspent output names something that is not a txid")?;

        coins.push((
            Outpoint {
                // Big-endian on the wire out of the API, as every hash a
                // person reads is; reversed back at this edge and nowhere else.
                txid: Txid::from_bytes(crate::util::display_order(txid)),
                v_out: number(coin, "index")? as u32,
            },
            Coin {
                output: TxOut {
                    value: Amount::from_atoms(number(coin, "atoms")?)?,
                    script_pubkey: script.clone(),
                },
                height: number(coin, "height")? as u32,
                from_coinbase: coin["coinbase"].as_bool().unwrap_or(false),
            },
        ));
    }

    Ok(UtxoSet::restored(coins))
}

/// AVI as a person writes it, into the atoms everything else counts in.
pub fn atoms_of(text: &str) -> Result<Amount> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if fraction.len() > 8 {
        bail!("{text} is finer than an atom");
    }

    let whole: u64 = whole
        .parse()
        .with_context(|| format!("{text} is not an amount"))?;
    let padded = format!("{fraction:0<8}");
    let fraction: u64 = if fraction.is_empty() {
        0
    } else {
        padded
            .parse()
            .with_context(|| format!("{text} is not an amount"))?
    };

    whole
        .checked_mul(ATOMS_PER_AVI)
        .and_then(|atoms| atoms.checked_add(fraction))
        .ok_or_else(|| anyhow!("{text} is more than there will ever be"))
        .and_then(Amount::from_atoms)
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .with_context(|| format!("the node's answer has no {field}"))
}

fn number(value: &Value, field: &str) -> Result<u64> {
    value[field]
        .as_u64()
        .with_context(|| format!("the node's answer has no {field}"))
}

fn ask(api: SocketAddr, path: &str) -> Result<Value> {
    request(api, "GET", path, None)
}

fn tell(api: SocketAddr, path: &str, body: &str) -> Result<Value> {
    request(api, "POST", path, Some(body))
}

/// HTTP/1.1, the little of it this needs, for the same reason `api.rs` writes
/// its own: the alternative is a dependency for four requests.
fn request(api: SocketAddr, method: &str, path: &str, body: Option<&str>) -> Result<Value> {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {api}\r\nConnection: close\r\n");
    if let Some(body) = body {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");

    let mut stream = TcpStream::connect_timeout(&api, PATIENCE)
        .with_context(|| format!("no node answering on {api}"))?;
    stream.set_read_timeout(Some(PATIENCE))?;
    stream.set_write_timeout(Some(PATIENCE))?;

    stream.write_all(head.as_bytes())?;
    stream.write_all(body.unwrap_or("").as_bytes())?;

    let mut answer = Vec::new();
    stream
        .take(MAX_ANSWER as u64)
        .read_to_end(&mut answer)
        .with_context(|| format!("{api} said nothing within {PATIENCE:?}"))?;

    let text = String::from_utf8_lossy(&answer);
    let (head, payload) = text
        .split_once("\r\n\r\n")
        .context("the node did not answer with HTTP")?;
    let parsed: Value = serde_json::from_str(payload).context("the node's answer is not JSON")?;

    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .context("the node's answer has no status")?;

    if status != 200 {
        // The node's reason, not ours. It knows why it refused and we do not.
        bail!(
            "{api} refused: {}",
            parsed["error"].as_str().unwrap_or(payload)
        );
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("1", ATOMS_PER_AVI)]
    #[case("1.5", 150_000_000)]
    #[case("0.00000001", 1)]
    #[case("0", 0)]
    #[case("50.00000099", 5_000_000_099)]
    fn an_amount_in_avi_becomes_the_atoms_everything_counts_in(
        #[case] written: &str,
        #[case] atoms: u64,
    ) {
        assert_eq!(atoms_of(written).unwrap().atoms(), atoms);
    }

    #[rstest]
    #[case::finer_than_an_atom("0.000000001")]
    #[case::not_a_number("lots")]
    #[case::empty("")]
    #[case::negative("-1")]
    #[case::two_points("1.2.3")]
    #[case::past_max_money("999999999")]
    fn an_amount_that_is_not_one_is_refused(#[case] written: &str) {
        assert!(atoms_of(written).is_err(), "{written}");
    }

    /// A hash the API prints is big-endian; an outpoint is not. Reversing it
    /// back belongs at this edge and nowhere else — invariant 5.
    #[test]
    fn an_unspent_output_is_read_back_into_the_outpoint_it_names() {
        let wallet = Wallet::new();
        let txid = Txid::from_bytes([7; 32]);
        let held = serde_json::json!({
            "unspent": [{
                "txid": txid.to_string(),
                "index": 3,
                "atoms": 5_000,
                "height": 9,
                "coinbase": true,
            }]
        });

        let utxo = coins_of(&held, &wallet).unwrap();
        let coin = utxo
            .get(&Outpoint { txid, v_out: 3 })
            .expect("the outpoint the API named");

        assert_eq!(coin.output.value.atoms(), 5_000);
        assert_eq!(coin.output.script_pubkey, p2pkh(&wallet.pubkey_hash()));
        assert_eq!(coin.height, 9);
        assert!(coin.from_coinbase);
    }

    #[test]
    fn an_answer_that_is_not_an_unspent_list_is_refused() {
        let wallet = Wallet::new();

        assert!(coins_of(&serde_json::json!({}), &wallet).is_err());
        assert!(coins_of(&serde_json::json!({"unspent": [{"txid": "nope"}]}), &wallet).is_err());
    }

    /// `send` must never mint a key. A key the node has not got is an address
    /// nobody will ever pay, and the coins would be lost the moment they were
    /// mined to it.
    #[test]
    fn a_missing_key_is_refused_rather_than_created() {
        let empty = std::env::temp_dir().join(format!("avicoin-send-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        let key = empty.join(KEY_FILE);
        std::fs::remove_file(&key).ok();

        let error = format!("{:#}", Wallet::read(&key).unwrap_err());

        assert!(error.contains("holds no wallet key"), "{error}");
        assert!(!key.exists(), "and nothing was written");
        std::fs::remove_dir_all(&empty).ok();
    }
}
