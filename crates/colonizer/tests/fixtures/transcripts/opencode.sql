-- The minimal OpenCode SQLite store (the schema txcript's own unit test builds):
-- one session, a user message, and an assistant turn with a completed tool call.
CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, title TEXT, version TEXT,
                      time_created INTEGER, time_archived INTEGER, model TEXT, slug TEXT, time_updated INTEGER);
CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
CREATE TABLE part    (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, data TEXT);
INSERT INTO session VALUES ('ses_1','/repo','Demo','1.15.0',1778834704515,NULL,'{"id":"claude-opus-4-7"}','clever-engine',1778834704516);
INSERT INTO message VALUES ('m_u','ses_1',1778834704520,'{"role":"user"}');
INSERT INTO part    VALUES ('p_u','m_u','ses_1','{"type":"text","text":"edit it"}');
INSERT INTO message VALUES ('m_a','ses_1',1778834704540,'{"role":"assistant","modelID":"claude-opus-4-7","finish":"stop","tokens":{"input":6,"output":88,"cache":{"write":21428,"read":10}}}');
INSERT INTO part    VALUES ('p_a1','m_a','ses_1','{"type":"reasoning","text":"thinking"}');
INSERT INTO part    VALUES ('p_a2','m_a','ses_1','{"type":"text","text":"On it."}');
INSERT INTO part    VALUES ('p_a3','m_a','ses_1','{"type":"tool","tool":"edit","callID":"c1","state":{"status":"completed","input":{"filePath":"/repo/a.rs","oldString":"old","newString":"new"},"output":"done"}}');
INSERT INTO part    VALUES ('p_a4','m_a','ses_1','{"type":"text","text":"Finished."}');
