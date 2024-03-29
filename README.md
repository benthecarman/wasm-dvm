# WASM DVM

## Introduction

WASM DVM is a WebAssembly-based [data vending machine](https://www.data-vending-machines.org/).

Currently, this uses [extism](https://extism.org/) as the execution environment. The Wasm code is executed in a
WebAssembly runtime environment. If you want to develop a wasm plugin, you can use the [extism](https://extism.org/)
PDK for developing and testing your wasm code.

## Currently Supported Features

- [x] Pay per time execution
- [x] Pre-paid execution with zaps
- [x] Encrypted input and output
- [x] Scheduled execution
- [x] DLC announcement based execution

## Nostr Events

### Input

Clients must provide the Wasm code in the `i` tag field. The Wasm code must be directly accessible at the provided URL.
It must also provide the input data and the provided function name to be executed.

The input should be a stringified JSON object with the following fields:

- `url` (string): The URL of the Wasm binary.
- `function` (string): The name of the function to be executed.
- `input` (string): The input data for the function.
- `time` (number): The maximum time in milliseconds to execute the function.
- `checksum` (string): The sha256 hash of the Wasm binary in hex.
- `shedule` (object): Scheduling parameters for the execution. The object should have the following fields:
    - `run_date` (number): The date in seconds since the epoch to execute the function.
    - `name` (optional string): Name of the event. Only used for DLC announcement
    - `expected_outputs` (optional string array): The list of expected outputs from the function. Only used for DLC
      announcement.

### Output

The result of the execution is returned in the `content` field.

### Example

Count number of vowels in a string.

#### Request

```json
{
  "content": "",
  "kind": 5600,
  "tags": [
    [
      "i",
      "{\"url\":\"https://github.com/extism/plugins/releases/download/v0.5.0/count_vowels.wasm\",\"function\":\"count_vowels\",\"input\":\"Hello World\",\"time\": 1000, \"checksum\": \"93898457953d30d016f712ccf4336ce7e9971db5f7f3aff1edd252764f75d5d7\"}",
      "text"
    ]
  ]
}
```

#### Response

```json
{
  "content": "{\"count\":3,\"total\":3,\"vowels\":\"aeiouAEIOU\"}",
  "kind": 6600
}
```
