pub struct Transaction {
    pub version: u32,
    pub inputs: Vec<TxIn>,
    pub outputs: Vec<TxOut>,
    pub signature: String,
}

impl Transaction {
    // this should be the transaction in raw bytes format serialized twice by sha256
    pub fn get_tx_id(self) {}

    pub fn get_raw_format(self) -> Vec<u8> {
        let mut raw_format = Vec::new();
        raw_format.extend(self.version.to_le_bytes());
        for tx in self.inputs {
            raw_format.extend(tx.previous_output.tx_id);
            raw_format.extend(tx.previous_output.v_out.to_le_bytes());
        }

        for tx in self.outputs {
            raw_format.extend(tx.value.to_le_bytes());
            raw_format.extend(tx.destiny_pub_key.as_bytes());
        }

        raw_format
    }
}

pub struct TxIn {
    pub previous_output: Outpoint,
}

pub struct Outpoint {
    pub tx_id: [u8; 32],
    pub v_out: u32,
}

pub struct TxOut {
    pub value: u64,
    pub destiny_pub_key: String,
}
