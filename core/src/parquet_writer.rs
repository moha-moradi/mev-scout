use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::*;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

use crate::data::{AccountData, BlockData, ReceiptData, TxData};

fn hash_col() -> Field {
    Field::new("hash", DataType::Binary, false)
}

fn addr_col(name: &str, nullable: bool) -> Field {
    Field::new(name, DataType::Binary, nullable)
}

fn blob64_col() -> Field {
    Field::new("value", DataType::Binary, false)
}

fn block_schema() -> Schema {
    Schema::new(vec![
        Field::new("number", DataType::Int64, false),
        hash_col(),
        Field::new("timestamp", DataType::Int64, false),
        Field::new("base_fee_per_gas", DataType::Int64, true),
        Field::new("gas_limit", DataType::Int64, false),
        Field::new("gas_used", DataType::Int64, false),
        addr_col("coinbase", false),
    ])
}

fn tx_schema() -> Schema {
    Schema::new(vec![
        hash_col(),
        Field::new("block_number", DataType::Int64, false),
        Field::new("tx_index", DataType::Int32, false),
        addr_col("from_addr", false),
        addr_col("to_addr", true),
        Field::new("input", DataType::Binary, false),
        blob64_col(),
        Field::new("gas_limit", DataType::Int64, false),
        Field::new("max_fee_per_gas", DataType::Int64, false),
        Field::new("max_priority_fee_per_gas", DataType::Int64, true),
        Field::new("nonce", DataType::Int64, false),
        Field::new("access_list", DataType::Binary, true),
    ])
}

fn receipt_schema() -> Schema {
    Schema::new(vec![
        hash_col(),
        Field::new("tx_index", DataType::Int32, false),
        Field::new("status", DataType::Boolean, false),
        Field::new("gas_used", DataType::Int64, false),
        Field::new("cumulative_gas_used", DataType::Int64, false),
        Field::new("logs", DataType::Binary, false),
        addr_col("contract_address", true),
    ])
}

fn account_schema() -> Schema {
    Schema::new(vec![
        Field::new("block_number", DataType::Int64, false),
        addr_col("address", false),
        Field::new("nonce", DataType::Int64, false),
        Field::new("balance", DataType::Binary, false),
        hash_col(),
    ])
}

fn serialize_bincode<T: serde::Serialize>(v: &T) -> anyhow::Result<Vec<u8>> {
    Ok(bincode::serialize(v)?)
}

struct BlockBatch {
    numbers: Int64Builder,
    hashes: BinaryBuilder,
    timestamps: Int64Builder,
    base_fees: Int64Builder,
    gas_limits: Int64Builder,
    gas_useds: Int64Builder,
    coinbases: BinaryBuilder,
}

impl BlockBatch {
    fn new() -> Self {
        Self {
            numbers: Int64Builder::new(),
            hashes: BinaryBuilder::new(),
            timestamps: Int64Builder::new(),
            base_fees: Int64Builder::new(),
            gas_limits: Int64Builder::new(),
            gas_useds: Int64Builder::new(),
            coinbases: BinaryBuilder::new(),
        }
    }

    fn append(&mut self, b: &BlockData) {
        self.numbers.append_value(b.number as i64);
        self.hashes.append_value(b.hash.as_slice());
        self.timestamps.append_value(b.timestamp as i64);
        match b.base_fee_per_gas {
            Some(v) => self.base_fees.append_value(v as i64),
            None => self.base_fees.append_null(),
        }
        self.gas_limits.append_value(b.gas_limit as i64);
        self.gas_useds.append_value(b.gas_used as i64);
        self.coinbases.append_value(b.coinbase.as_slice());
    }

    fn finish(mut self) -> anyhow::Result<RecordBatch> {
        let schema = Arc::new(block_schema());
        let columns: Vec<Arc<dyn Array>> = vec![
            Arc::new(self.numbers.finish()),
            Arc::new(self.hashes.finish()),
            Arc::new(self.timestamps.finish()),
            Arc::new(self.base_fees.finish()),
            Arc::new(self.gas_limits.finish()),
            Arc::new(self.gas_useds.finish()),
            Arc::new(self.coinbases.finish()),
        ];
        Ok(RecordBatch::try_new(schema, columns)?)
    }
}

struct TxBatch {
    hashes: BinaryBuilder,
    block_numbers: Int64Builder,
    tx_indices: Int32Builder,
    from_addrs: BinaryBuilder,
    to_addrs: BinaryBuilder,
    inputs: BinaryBuilder,
    values: BinaryBuilder,
    gas_limits: Int64Builder,
    max_fees: Int64Builder,
    max_priority_fees: Int64Builder,
    nonces: Int64Builder,
    access_lists: BinaryBuilder,
}

impl TxBatch {
    fn new() -> Self {
        Self {
            hashes: BinaryBuilder::new(),
            block_numbers: Int64Builder::new(),
            tx_indices: Int32Builder::new(),
            from_addrs: BinaryBuilder::new(),
            to_addrs: BinaryBuilder::new(),
            inputs: BinaryBuilder::new(),
            values: BinaryBuilder::new(),
            gas_limits: Int64Builder::new(),
            max_fees: Int64Builder::new(),
            max_priority_fees: Int64Builder::new(),
            nonces: Int64Builder::new(),
            access_lists: BinaryBuilder::new(),
        }
    }

    fn append(&mut self, block_num: u64, tx: &TxData) {
        self.hashes.append_value(tx.hash.as_slice());
        self.block_numbers.append_value(block_num as i64);
        self.tx_indices.append_value(tx.index as i32);
        self.from_addrs.append_value(tx.from.as_slice());
        match tx.to {
            Some(addr) => self.to_addrs.append_value(addr.as_slice()),
            None => self.to_addrs.append_null(),
        }
        self.inputs.append_value(tx.input.as_ref());
        self.values.append_value(&tx.value.to_be_bytes::<32>());
        self.gas_limits.append_value(tx.gas_limit as i64);
        self.max_fees.append_value(tx.max_fee_per_gas as i64);
        match tx.max_priority_fee_per_gas {
            Some(v) => self.max_priority_fees.append_value(v as i64),
            None => self.max_priority_fees.append_null(),
        }
        self.nonces.append_value(tx.nonce as i64);
        if tx.access_list.is_empty() {
            self.access_lists.append_null();
        } else {
            self.access_lists
                .append_value(&serialize_bincode(&tx.access_list).unwrap_or_default());
        }
    }

    fn finish(mut self) -> anyhow::Result<RecordBatch> {
        let schema = Arc::new(tx_schema());
        let columns: Vec<Arc<dyn Array>> = vec![
            Arc::new(self.hashes.finish()),
            Arc::new(self.block_numbers.finish()),
            Arc::new(self.tx_indices.finish()),
            Arc::new(self.from_addrs.finish()),
            Arc::new(self.to_addrs.finish()),
            Arc::new(self.inputs.finish()),
            Arc::new(self.values.finish()),
            Arc::new(self.gas_limits.finish()),
            Arc::new(self.max_fees.finish()),
            Arc::new(self.max_priority_fees.finish()),
            Arc::new(self.nonces.finish()),
            Arc::new(self.access_lists.finish()),
        ];
        Ok(RecordBatch::try_new(schema, columns)?)
    }
}

struct ReceiptBatch {
    tx_hashes: BinaryBuilder,
    tx_indices: Int32Builder,
    statuses: BooleanBuilder,
    gas_useds: Int64Builder,
    cumulative_gas_useds: Int64Builder,
    logs: BinaryBuilder,
    contract_addrs: BinaryBuilder,
}

impl ReceiptBatch {
    fn new() -> Self {
        Self {
            tx_hashes: BinaryBuilder::new(),
            tx_indices: Int32Builder::new(),
            statuses: BooleanBuilder::new(),
            gas_useds: Int64Builder::new(),
            cumulative_gas_useds: Int64Builder::new(),
            logs: BinaryBuilder::new(),
            contract_addrs: BinaryBuilder::new(),
        }
    }

    fn append(&mut self, r: &ReceiptData) {
        self.tx_hashes.append_value(r.tx_hash.as_slice());
        self.tx_indices.append_value(r.tx_index as i32);
        self.statuses.append_value(r.status);
        self.gas_useds.append_value(r.gas_used as i64);
        self.cumulative_gas_useds
            .append_value(r.cumulative_gas_used as i64);
        self.logs
            .append_value(&serialize_bincode(&r.logs).unwrap_or_default());
        match r.contract_address {
            Some(addr) => self.contract_addrs.append_value(addr.as_slice()),
            None => self.contract_addrs.append_null(),
        }
    }

    fn finish(mut self) -> anyhow::Result<RecordBatch> {
        let schema = Arc::new(receipt_schema());
        let columns: Vec<Arc<dyn Array>> = vec![
            Arc::new(self.tx_hashes.finish()),
            Arc::new(self.tx_indices.finish()),
            Arc::new(self.statuses.finish()),
            Arc::new(self.gas_useds.finish()),
            Arc::new(self.cumulative_gas_useds.finish()),
            Arc::new(self.logs.finish()),
            Arc::new(self.contract_addrs.finish()),
        ];
        Ok(RecordBatch::try_new(schema, columns)?)
    }
}

struct AccountBatch {
    block_numbers: Int64Builder,
    addrs: BinaryBuilder,
    nonces: Int64Builder,
    balances: BinaryBuilder,
    code_hashes: BinaryBuilder,
}

impl AccountBatch {
    fn new() -> Self {
        Self {
            block_numbers: Int64Builder::new(),
            addrs: BinaryBuilder::new(),
            nonces: Int64Builder::new(),
            balances: BinaryBuilder::new(),
            code_hashes: BinaryBuilder::new(),
        }
    }

    fn append(&mut self, block_num: u64, addr: &[u8], acc: &AccountData) {
        self.block_numbers.append_value(block_num as i64);
        self.addrs.append_value(addr);
        self.nonces.append_value(acc.nonce as i64);
        self.balances.append_value(&acc.balance.to_be_bytes::<32>());
        self.code_hashes.append_value(acc.code_hash.as_slice());
    }

    fn finish(mut self) -> anyhow::Result<RecordBatch> {
        let schema = Arc::new(account_schema());
        let columns: Vec<Arc<dyn Array>> = vec![
            Arc::new(self.block_numbers.finish()),
            Arc::new(self.addrs.finish()),
            Arc::new(self.nonces.finish()),
            Arc::new(self.balances.finish()),
            Arc::new(self.code_hashes.finish()),
        ];
        Ok(RecordBatch::try_new(schema, columns)?)
    }
}

/// Parquet file writer for intermediate block/transaction/receipt/account storage.
///
/// Writes one Parquet file per data type per directory. Each data type directory
/// contains a single file that is appended to on each write call.
/// Files are stored under `{parquet_dir}/{type}/data.parquet`.
pub struct ParquetWriter {
    dir: PathBuf,
}

impl ParquetWriter {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
        }
    }

    fn file_path(&self, kind: &str) -> PathBuf {
        self.dir.join(kind).join("data.parquet")
    }

    fn write_batch(batch: &RecordBatch, path: &Path, append: bool) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(append)
            .read(false)
            .write(true)
            .open(path)?;
        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .build();
        let mut writer = ArrowWriter::try_new(file, batch.schema(), Some(props))?;
        writer.write(batch)?;
        writer.close()?;
        Ok(())
    }

    pub fn write_all_blocks(&self, blocks: &[BlockData]) -> anyhow::Result<()> {
        let path = self.file_path("blocks");
        let mut batch_builder = BlockBatch::new();
        for b in blocks {
            batch_builder.append(b);
        }
        let batch = batch_builder.finish()?;
        Self::write_batch(&batch, &path, false)?;
        Ok(())
    }

    pub fn write_all_txs(&self, txs: &[Vec<TxData>]) -> anyhow::Result<()> {
        let path = self.file_path("txs");
        let mut batch_builder = TxBatch::new();
        for (block_num, block_txs) in txs.iter().enumerate() {
            for tx in block_txs {
                batch_builder.append(block_num as u64, tx);
            }
        }
        let batch = batch_builder.finish()?;
        Self::write_batch(&batch, &path, false)?;
        Ok(())
    }

    pub fn write_all_receipts(&self, receipts: &[Vec<ReceiptData>]) -> anyhow::Result<()> {
        let path = self.file_path("receipts");
        let mut batch_builder = ReceiptBatch::new();
        for block_receipts in receipts {
            for r in block_receipts {
                batch_builder.append(r);
            }
        }
        let batch = batch_builder.finish()?;
        Self::write_batch(&batch, &path, false)?;
        Ok(())
    }

    pub fn write_all_accounts(
        &self,
        accounts: &[(u64, alloy::primitives::Address, AccountData)],
    ) -> anyhow::Result<()> {
        let path = self.file_path("accounts");
        let mut batch_builder = AccountBatch::new();
        for (block_num, addr, acc) in accounts {
            batch_builder.append(*block_num, addr.as_slice(), acc);
        }
        let batch = batch_builder.finish()?;
        Self::write_batch(&batch, &path, false)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::LogData;
    use alloy::primitives::{address, b256, B256, U256};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("mev_pq_test_{n}"))
    }

    fn sample_block(num: u64) -> BlockData {
        BlockData {
            number: num,
            hash: B256::from_slice(&[num as u8; 32]),
            timestamp: 1000 + num,
            base_fee_per_gas: Some(50_000_000_000),
            gas_limit: 30_000_000,
            gas_used: 15_000_000,
            coinbase: address!("dead000000000000000000000000000000000000"),
        }
    }

    fn sample_tx(block_num: u64, idx: u64) -> TxData {
        TxData {
            hash: B256::from_slice(&[(block_num + idx) as u8; 32]),
            index: idx,
            from: address!("aa00000000000000000000000000000000000000"),
            to: Some(address!("bb00000000000000000000000000000000000000")),
            input: vec![0x12, 0x34].into(),
            value: U256::from(1000u64),
            gas_limit: 100_000,
            max_fee_per_gas: 100_000_000_000,
            max_priority_fee_per_gas: Some(1_000_000_000),
            nonce: 5,
            access_list: vec![],
        }
    }

    fn sample_receipt(tx_hash: B256, idx: u64) -> ReceiptData {
        ReceiptData {
            tx_hash,
            tx_index: idx,
            status: true,
            gas_used: 50_000,
            cumulative_gas_used: 50_000,
            logs: vec![LogData {
                address: address!("cafe000000000000000000000000000000000000"),
                topics: vec![b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822")],
                data: vec![0xab, 0xcd].into(),
            }],
            contract_address: None,
        }
    }

    #[test]
    fn test_write_read_blocks() {
        let dir = temp_dir();
        let pw = ParquetWriter::new(&dir);
        let blocks = vec![sample_block(1), sample_block(2), sample_block(3)];
        pw.write_all_blocks(&blocks).unwrap();
        let path = pw.file_path("blocks");
        assert!(path.exists(), "blocks parquet file should exist");
        let metadata = std::fs::metadata(&path).unwrap();
        assert!(metadata.len() > 0, "parquet file should not be empty");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_write_read_txs() {
        let dir = temp_dir();
        let pw = ParquetWriter::new(&dir);
        let txs = vec![
            vec![sample_tx(1, 0), sample_tx(1, 1)],
            vec![sample_tx(2, 0)],
        ];
        pw.write_all_txs(&txs).unwrap();
        let path = pw.file_path("txs");
        assert!(path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_write_read_receipts() {
        let dir = temp_dir();
        let pw = ParquetWriter::new(&dir);
        let h1 = sample_tx(1, 0).hash;
        let h2 = sample_tx(1, 1).hash;
        let receipts = vec![
            vec![sample_receipt(h1, 0), sample_receipt(h2, 1)],
        ];
        pw.write_all_receipts(&receipts).unwrap();
        let path = pw.file_path("receipts");
        assert!(path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_write_accounts() {
        let dir = temp_dir();
        let pw = ParquetWriter::new(&dir);
        let addr = address!("abcd000000000000000000000000000000000001");
        let acc = AccountData {
            nonce: 10,
            balance: U256::from(1_000_000_000u64),
            code_hash: b256!("0000000000000000000000000000000000000000000000000000000000000004"),
        };
        let accounts = vec![(42u64, addr, acc)];
        pw.write_all_accounts(&accounts).unwrap();
        let path = pw.file_path("accounts");
        assert!(path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
