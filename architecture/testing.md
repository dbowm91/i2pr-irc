# Deterministic testing

`ScriptedStream` bounds write capacity and configures short reads/writes. Provider outcomes and requested I2P endpoints can be scripted. The fuzz smoke executable deterministically derives arbitrary bounded byte inputs and exercises the parser and incremental decoder without network access or wall-clock sleeps.

The current M002 runtime is only a registration attempt; online liveness, downstream serving, generation event routing, reconnect scheduling, and shutdown joins are not yet qualified.
