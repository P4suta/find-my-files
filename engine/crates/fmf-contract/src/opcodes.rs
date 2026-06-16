//! Pipe opcodes (docs/ARCHITECTURE.md オペコード表). Event pushes reuse
//! 1..=6 as the event *kind* with `flags = event` — dispatch must branch on
//! the flag before the opcode.

/// `Hello`: 接続ハンドシェイク・版交渉(`fmf_abi_version` に対応)。
pub const HELLO: u16 = 1;
/// `Subscribe`: 以後この接続へイベントをプッシュ(`fmf_set_event_callback(cb≠NULL)` に対応)。
pub const SUBSCRIBE: u16 = 2;
/// `Unsubscribe`: イベントプッシュを停止(`fmf_set_event_callback(NULL)` に対応)。
pub const UNSUBSCRIBE: u16 = 3;
/// `ListVolumes`: 全ボリュームの状態とエントリ数を返す(`fmf_list_volumes` に対応)。
pub const LIST_VOLUMES: u16 = 4;
/// `IndexStart`: 指定ボリュームの索引を開始(`fmf_index_start` に対応・service.json へ永続化)。
pub const INDEX_START: u16 = 5;
/// `IndexStatus`: 索引進捗・状態を返す(`fmf_index_status` に対応・`ListVolumes` と同形)。
pub const INDEX_STATUS: u16 = 6;
/// `Query`: クエリを実行し `result_id` と件数を返す(`fmf_query` に対応)。
pub const QUERY: u16 = 7;
/// `ResultPage`: `result_id` の結果から行ページを取得(`fmf_result_page` に対応)。
pub const RESULT_PAGE: u16 = 8;
/// `ResultFree`: `result_id` の結果ハンドルを解放(`fmf_result_free` に対応)。
pub const RESULT_FREE: u16 = 9;
/// `Stats`: エンジンのメトリクススナップショットを返す(`fmf_engine_stats` に対応)。
pub const STATS: u16 = 10;
/// Number reserved, deliberately unimplemented (client-driven flush is a
/// local-DoS lever — ADR-0016).
pub const FLUSH_RESERVED: u16 = 11;
/// `ServiceInfo`: サービス固有の稼働情報を返す(`uptime_ms` / connections / version)。
pub const SERVICE_INFO: u16 = 12;
