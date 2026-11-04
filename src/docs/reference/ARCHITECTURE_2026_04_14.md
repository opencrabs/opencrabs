# OpenCrabs System Architecture

## 1. High-Level Overview

```mermaid
flowchart TB
    TUI[TUI Interactive] --> AGENT[AgentService]
    CLI[CLI Noninteractive] --> AGENT
    DAEMON[Daemon Headless] --> CM[Channel Manager]
    A2A[A2A Gateway] --> AGENT
    TG[Telegram] --> CM
    DC[Discord] --> CM
    SL[Slack] --> CM
    WA[WhatsApp] --> CM
    TR[Trello] --> CM
    VO[Voice STT TTS] --> CM
    CM --> AGENT
    AGENT --> BRAIN[BrainLoader]
    AGENT --> TOOLS[ToolRegistry]
    AGENT --> PROVIDERS[Provider System]
    AGENT --> SELFHEAL[Self Healing Engine]
    AGENT --> SUBAGENT[Sub Agent Manager]
    AGENT --> DB[(SQLite)]
    AGENT --> MEM[(Memory Store)]
    SELFHEAL --> CONTEXT[Context Management]
    SELFHEAL --> DETECTION[Detection and Recovery]
    SELFHEAL --> PROVRECOV[Provider Recovery]
    RSI[RSI Engine] --> FBLEDGER[(Feedback Ledger)]
    RSI --> RSIAGENT[RSI Agent]
    RSIAGENT --> BRAIN
    CRON[Cron Scheduler] --> AGENT
```

## 2. Self Healing Engine

```mermai22d
flowchart TB
    AGENT[AgentService Tool Loop] --> CTX[Context Management]
    AGENT --> DET[Detection]
    AGENT --> PROV[Provider Recovery]
    AGENT --> PERSIST[Persistence]
    CTX --> SOFT[Soft Compaction at 65 pct]
    SOFT --> KEEP[LLM summarizes to 55 pct]
    CTX --> HARD[Hard Truncation at 90 pct]
    HARD --> DROP[Drop oldest to 80 pct]
    DROP --> SOFT
    CTX --> EMERG[Emergency Compaction]
    EMERG --> PRETRUNC[Pre truncate to 85 pct]
    PRETRUNC --> SOFT
    CTX --> CALIB[Token Calibration from provider]
    CTX --> MARKER[Compaction Marker Recovery]
    DET --> PHANTOM[Phantom Tool Detection]
    PHANTOM --> CORRECTION[Inject correction and retry]
    DET --> GASLIGHT[Gaslighting Preamble Strip]
    GASLIGHT --> STRIPPARA[Strip leading paragraphs]
    DET --> REPET[Text Repetition Detection]
    REPET --> CANCELSTREAM[Cancel stream and retry]
    DET --> LOOPDET[Tool Loop Detection]
    LOOPDET --> BREAKLOOP[Break after 4 to 8 repeats]
    DET --> USERCORR[User Correction Detection]
    USERCORR --> RECORDFB[Record to feedback ledger]
    DET --> XMLRECOV[XML Tool Call Recovery]
    XMLRECOV --> SYNTH[Synthesize ToolUse blocks]
    DET --> HTMLSTRIP[HTML Comment Stripping]
    PROV --> RATELIMIT[Rate Limit Handler]
    RATELIMIT --> WALKCHAIN[Walk fallback chain]
    PROV --> STREAMERR[Stream Error Handler]
    STREAMERR --> RETRY5[Retry 5x with backoff]
    RETRY5 --> WALKCHAIN
    PROV --> STREAMDROP[Stream Drop Handler]
    STREAMDROP --> DROPRETRY[Retry 5x then fallback]
    PROV --> ROTCONT[Rotation Continuation]
    ROTCONT --> INJECTCONT[Inject continuation prompt]
    PROV --> SWAPEVENT[Sticky Fallback Swap]
    SWAPEVENT --> NOTIFY[SwapEvent to TUI]
    PERSIST --> ATOMIC[Atomic Message Writes]
    PERSIST --> CRASHTRACK[Crash Recovery Tracking]
    PERSIST --> QUEUED[Queued Message Injection]
    PERSIST --> SESSMODEL[Session Model Fallback]
    PERSIST --> MARKERSTRIP[LLM Artifact Stripping]
```

## 3. RSI Recursive Self Improvement

```mermaid
flowchart TD
    STARTUP([App Startup]) --> READTS[Read last cycle timestamp]
    READTS --> CALC[Calculate remaining delay]
    CALC --> SLEEP[Sleep remaining time]
    SLEEP --> QUERY[Query feedback ledger total]
    QUERY --> MIN{50 plus entries}
    MIN --> |no|STAMP[Stamp last cycle file]
    STAMP --> SLEEP
    MIN --> |yes|CHANGED{Count changed}
    CHANGED --> |no|STAMP
    CHANGED --> |yes|DETECT[Detect Opportunities]
    DETECT --> TOOLFAIL[Tool failure rate over 40 pct]
    DETECT --> USERCORR[3 plus user corrections]
    DETECT --> PROVERR[3 plus provider errors]
    TOOLFAIL --> OPP[Opportunities with session model timestamps]
    USERCORR --> OPP
    PROVERR --> OPP
    OPP --> HAS{Any found}
    HAS --> |no|STAMP
    HAS --> |yes|SPAWN[Spawn RSI Agent]
    SPAWN --> FA[feedback analyze]
    FA --> DECIDE{Fixable via brain files}
    DECIDE --> |yes|READBRAIN[self improve read]
    READBRAIN --> APPLY[self improve apply or update]
    APPLY --> LOG[Log to improvements md]
    LOG --> ARCHIVE[Archive to history]
    DECIDE --> |no|GHCHECK{GitHub tool available}
    GHCHECK --> |yes|GHLIST[List existing issues]
    GHLIST --> GHCREATE[Create if no duplicate]
    GHCHECK --> |no|SKIP[Skip]
    ARCHIVE --> STAMP
    GHCREATE --> STAMP
    SKIP --> STAMP
    APPLY --> SOUL[SOUL md]
    APPLY --> TOOLSMD[TOOLS md]
    APPLY --> USERMD[USER md]
    APPLY --> AGENTSMD[AGENTS md]
    APPLY --> CODEMD[CODE md]
    APPLY --> SECURITYMD[SECURITY md]
```

## 4. Provider System and Fallback Chain

```mermaid
flowchart TD
    REQ([LLM Request]) --> FB[FallbackProvider]
    FB --> REMAP[Model Remapping]
    REMAP --> ACTIVE[Active Provider]
    ACTIVE --> |success|RESP([Response])
    ACTIVE --> |error|WALK[Walk Fallback Chain]
    WALK --> P1[Provider 2]
    P1 --> |success|PROM1[Sticky Promotion]
    P1 --> |fail|P2[Provider 3]
    P2 --> |success|PROM2[Sticky Promotion]
    P2 --> |fail|P3[Provider N]
    P3 --> |success|PROM3[Sticky Promotion]
    P3 --> |all fail|ERR([Return Error])
    PROM1 --> RESP
    PROM2 --> RESP
    PROM3 --> RESP
    ANTH[Anthropic] --> FB
    QWEN[Qwen OAuth] --> FB
    GEMINI[Gemini] --> FB
    OPENAI[OpenAI] --> FB
    CUSTOM[Custom OpenAI compat] --> FB
    CCLI[Claude CLI] --> FB
    OCLI[OpenCode CLI] --> FB
    QCLI[Qwen Code CLI] --> FB
```

## 5. Qwen OAuth Rotation

```mermaid
flowchart TD
    REQ([Request]) --> ROT[RotatingQwenProvider]
    ROT --> A0[Account 0]
    A0 --> |success|RESP([Response])
    A0 --> |429|A1[Account 1]
    A1 --> |success|RESP
    A1 --> |429|A2[Account 2]
    A2 --> |success|RESP
    A2 --> |429|AN[Account N]
    AN --> |all 429|EXHAUST[All Exhausted]
    EXHAUST --> FALLBACK[Return error to FallbackProvider]
    TM0[TokenManager 0] --> A0
    TM1[TokenManager 1] --> A1
    TM2[TokenManager 2] --> A2
    TM0 --> BG0[Background refresh]
    TM1 --> BG1[Background refresh]
    TM2 --> BG2[Background refresh]
    BG0 --> SLOT0[Persist to slot 0]
    BG1 --> SLOT1[Persist to slot 1]
    BG2 --> SLOT2[Persist to slot 2]
```

## 6. Tool Loop

```mermaid
flowchart TD
    START([User Message]) --> BUDGET{Over 90 pct}
    BUDGET --> |yes|HARD[Hard Truncate to 80 pct]
    HARD --> RECHECK{Over 65 pct}
    RECHECK --> |yes|COMPACT[LLM Compaction to 55 pct]
    RECHECK --> |no|BUILD
    BUDGET --> |65 to 89|COMPACT
    COMPACT --> BUILD
    BUDGET --> |under 65|BUILD[Build LLM Request]
    BUILD --> STREAM[Provider stream]
    STREAM --> |success|PARSE[Parse Response]
    STREAM --> |rate limit|RATE[Walk Fallback Chain]
    STREAM --> |stream error|SRETRY[Retry 5x then Fallback]
    STREAM --> |prompt too long|EMERG[Emergency Compaction]
    STREAM --> |drop|DRETRY[Retry 5x then Fallback]
    STREAM --> |repetition|CANCEL[Cancel and Retry]
    RATE --> PARSE
    SRETRY --> PARSE
    DRETRY --> PARSE
    CANCEL --> SRETRY
    EMERG --> BUILD
    PARSE --> GASCHK{Gaslighting}
    GASCHK --> |yes|STRIP[Strip preamble]
    GASCHK --> |no|TOOLCHK
    STRIP --> TOOLCHK
    TOOLCHK{Has tool calls}
    TOOLCHK --> |yes|XMLCHK{XML blocks}
    TOOLCHK --> |no|PHANTCHK
    XMLCHK --> |yes|XMLPARSE[Parse XML to ToolUse]
    XMLCHK --> |no|EXEC
    XMLPARSE --> EXEC
    EXEC[Execute Each Tool] --> APPROVE{Needs approval}
    APPROVE --> |yes|ASKUSER[Ask User]
    ASKUSER --> |ok|RUNTOOL[Run Tool]
    ASKUSER --> |denied|DENIED[Return denied]
    APPROVE --> |no|RUNTOOL
    RUNTOOL --> RECORD[Record feedback]
    RECORD --> LOOPCHK{Same call 4 to 8x}
    LOOPCHK --> |yes|BREAK([Break Loop])
    LOOPCHK --> |no|PERSIST[Persist to DB]
    PERSIST --> QUEUECHK{Queued message}
    QUEUECHK --> |yes|INJECT[Inject user message]
    QUEUECHK --> |no|BUDGET
    INJECT --> BUDGET
    PHANTCHK{Phantom detected}
    PHANTCHK --> |yes first|PHANTRETRY[Inject correction]
    PHANTRETRY --> BUDGET
    PHANTCHK --> |no|ROTCHK{Provider rotated}
    ROTCHK --> |yes first|ROTRETRY[Inject continuation]
    ROTRETRY --> BUDGET
    ROTCHK --> |no|DONE([Return Response])
    PHANTCHK --> |retried|DONE
```

## 7. Sub Agents and Teams

```mermaid
flowchart TD
    PARENT[Parent Agent] --> |spawn|FORK[Fork Context]
    FORK --> C1[Child 1]
    FORK --> C2[Child 2]
    FORK --> C3[Child 3]
    PARENT --> |send input|C1
    PARENT --> |wait|C2
    PARENT --> |close|C3
    PARENT --> |resume|C1
    PARENT --> |team create|TEAM[Create Team]
    TEAM --> SPAWNN[Spawn N Agents]
    SPAWNN --> BCAST[Broadcast prompt to all]
    BCAST --> COLLECT[Collect responses]
    COLLECT --> AGG[Aggregate]
    AGG --> DELETE[Delete team]
```

## 8. Data Layer

```mermaid
flowchart LR
    subgraph SQLite
        sessions
        messages
        feedback_ledger
        usage_ledger
        plans
        cron_jobs
        channel_messages
        files
    end
    subgraph MemoryStore
        VEC[Vector Embeddings]
        FTS[FTS5 Full Text]
        VEC --> RRF[Hybrid RRF Ranking]
        FTS --> RRF
    end
    subgraph ConfigFS
        configtoml[config toml]
        keystoml[keys toml]
        brainfiles[Brain Files x8]
        rsidir[RSI improvements and history]
        profiles[Profile isolation]
    end
```

## 9. Channel Integration

```mermaid
flowchart TD
    MSG([Incoming Message]) --> CM[Channel Manager]
    CM --> AUTH{Allowed user}
    AUTH --> |no|DROP[Drop]
    AUTH --> |yes|SESS{Existing session}
    SESS --> |yes|LOAD[Load session]
    SESS --> |no|CREATE[Create session]
    LOAD --> AGENT[AgentService]
    CREATE --> AGENT
    AGENT --> REPLY[Response]
    REPLY --> PERSISTDB[Persist to DB]
    PERSISTDB --> SEND[Send to channel]
    TG[Telegram] --> CM
    DC[Discord] --> CM
    SL[Slack] --> CM
    WA[WhatsApp] --> CM
    TR[Trello] --> CM
    VO[Voice] --> CM
```

## 10. A2A Protocol

```mermaid
flowchart LR
    CLIENT([Remote Agent]) --> SERVER[Axum HTTP Server]
    SERVER --> AUTHCHK{Token valid}
    AUTHCHK --> |no|REJECT[401]
    AUTHCHK --> |yes|HANDLER[JSON RPC Handler]
    HANDLER --> MSGSEND[message send]
    HANDLER --> TASKGET[tasks get]
    HANDLER --> TASKCANCEL[tasks cancel]
    MSGSEND --> STORE[TaskStore]
    STORE --> AGENT[AgentService]
    AGENT --> RESULT[Task Result]
    RESULT --> STORE
    TASKGET --> STORE
    DISC([Any Client]) --> CARD[Agent Card]
```
