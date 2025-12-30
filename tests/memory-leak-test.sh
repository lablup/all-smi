#!/bin/bash

# Memory Leak Test Script for all-smi API mode
#
# This script monitors memory usage while repeatedly querying the API endpoint.
# It helps detect memory leaks by tracking RSS growth over time.
#
# Usage:
#   ./tests/memory-leak-test.sh [PORT] [DURATION_SECONDS]
#
# Example:
#   ./target/release/all-smi api --port 19090 &
#   ./tests/memory-leak-test.sh 19090 60

PORT=${1:-19090}
DURATION=${2:-60}
QUERY_INTERVAL=0.1
MONITOR_INTERVAL=1

PID=$(pgrep -f "all-smi api" | head -1)

if [ -z "$PID" ]; then
    echo "Error: all-smi api process not found"
    echo "Please start all-smi in API mode first:"
    echo "  ./target/release/all-smi api --port $PORT &"
    exit 1
fi

echo "=========================================="
echo "Memory Leak Test for all-smi API mode"
echo "=========================================="
echo "PID: $PID"
echo "Port: $PORT"
echo "Duration: ${DURATION}s"
echo "=========================================="

MEMORY_LOG=$(mktemp)
QUERY_LOG=$(mktemp)

INITIAL_RSS=$(ps -o rss= -p $PID | tr -d ' ')
echo "Initial RSS: ${INITIAL_RSS} KB"
echo ""

# Memory monitoring
(
    START_TIME=$(date +%s)
    while true; do
        CURRENT_TIME=$(date +%s)
        ELAPSED=$((CURRENT_TIME - START_TIME))
        [ $ELAPSED -ge $DURATION ] && break
        RSS=$(ps -o rss= -p $PID 2>/dev/null | tr -d ' ')
        [ -n "$RSS" ] && echo "$ELAPSED $RSS" >> "$MEMORY_LOG"
        sleep $MONITOR_INTERVAL
    done
) &
MONITOR_PID=$!

# Query loop
(
    QUERY_COUNT=0
    START_TIME=$(date +%s)
    while true; do
        CURRENT_TIME=$(date +%s)
        ELAPSED=$((CURRENT_TIME - START_TIME))
        [ $ELAPSED -ge $DURATION ] && break
        curl -s "http://localhost:$PORT/metrics" > /dev/null 2>&1
        QUERY_COUNT=$((QUERY_COUNT + 1))
        [ $((QUERY_COUNT % 100)) -eq 0 ] && echo "Queries: $QUERY_COUNT, Elapsed: ${ELAPSED}s" >> "$QUERY_LOG"
        sleep $QUERY_INTERVAL
    done
    echo "Total queries: $QUERY_COUNT" >> "$QUERY_LOG"
) &
QUERY_PID=$!

echo "Running test..."
for i in $(seq 1 $DURATION); do
    sleep 1
    RSS=$(ps -o rss= -p $PID 2>/dev/null | tr -d ' ')
    [ -n "$RSS" ] && printf "\r[%3d/%3ds] RSS: %d KB (D %+d KB)" $i $DURATION $RSS $((RSS - INITIAL_RSS))
done
echo ""
echo ""

wait $MONITOR_PID 2>/dev/null
wait $QUERY_PID 2>/dev/null

FINAL_RSS=$(ps -o rss= -p $PID | tr -d ' ')

echo "=========================================="
echo "Test Results"
echo "=========================================="
echo "Initial RSS: ${INITIAL_RSS} KB"
echo "Final RSS:   ${FINAL_RSS} KB"
echo "Difference:  $((FINAL_RSS - INITIAL_RSS)) KB"
echo ""

if [ -f "$MEMORY_LOG" ]; then
    MIN_RSS=$(awk '{print $2}' "$MEMORY_LOG" | sort -n | head -1)
    MAX_RSS=$(awk '{print $2}' "$MEMORY_LOG" | sort -n | tail -1)
    AVG_RSS=$(awk '{sum+=$2; count++} END {printf "%.0f", sum/count}' "$MEMORY_LOG")
    echo "Statistics:"
    echo "  Min RSS: ${MIN_RSS} KB"
    echo "  Max RSS: ${MAX_RSS} KB"
    echo "  Avg RSS: ${AVG_RSS} KB"
    echo "  Range:   $((MAX_RSS - MIN_RSS)) KB"
    echo ""
fi

cat "$QUERY_LOG"
echo ""

FIRST_AVG=$(head -5 "$MEMORY_LOG" | awk '{sum+=$2; count++} END {printf "%.0f", sum/count}')
LAST_AVG=$(tail -5 "$MEMORY_LOG" | awk '{sum+=$2; count++} END {printf "%.0f", sum/count}')
TREND=$((LAST_AVG - FIRST_AVG))

echo "=========================================="
echo "Trend Analysis"
echo "=========================================="
echo "First 5 samples avg: ${FIRST_AVG} KB"
echo "Last 5 samples avg:  ${LAST_AVG} KB"
echo "Trend:               ${TREND} KB"
echo ""

if [ $TREND -gt 1000 ]; then
    echo "WARNING: Memory appears to be increasing significantly!"
    echo "This could indicate a memory leak."
    EXIT_CODE=1
elif [ $TREND -gt 100 ]; then
    echo "NOTICE: Memory shows some increase."
    echo "Consider running a longer test to confirm stability."
    EXIT_CODE=0
else
    echo "Memory appears stable."
    EXIT_CODE=0
fi

rm -f "$MEMORY_LOG" "$QUERY_LOG"
exit $EXIT_CODE
