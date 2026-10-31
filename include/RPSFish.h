// Copyright (C) 2026 Henry Abrahamsen
// SPDX-License-Identifier: LGPL-3.0-or-later

#ifndef RPSFISH_H
#define RPSFISH_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// ABI constants. Check rpsfish_abi_version() before using the library.
enum {
  RPSFISH_ABI_VERSION = 4,
  RPSFISH_MODE_TOTAL_WAR = 1,
  RPSFISH_MODE_INFILTRATION = 2,
  RPSFISH_COLOR_RED = 0,
  RPSFISH_COLOR_BLUE = 1,
  RPSFISH_MAX_VARIATIONS = 8,
  RPSFISH_MAX_PV_LENGTH = 127,
  RPSFISH_ERROR_INVALID_POSITION = -2
};

typedef enum RPSFishStopReason {
  RPSFISH_STOP_DEPTH_LIMIT = 0,
  RPSFISH_STOP_NODE_LIMIT = 1,
  RPSFISH_STOP_TIME_LIMIT = 2,
  RPSFISH_STOP_CANCELLED = 3,
  RPSFISH_STOP_TERMINAL_POSITION = 4,
  RPSFISH_STOP_REPETITION = 5,
  RPSFISH_STOP_NO_LEGAL_MOVE = 6
} RPSFishStopReason;

int32_t rpsfish_abi_version(void);
int32_t rpsfish_rules_version(void);

// Search a position. Each u128 board crosses C as low then high uint64_t.
// Piece boards are Red Rock/Paper/Scissors then Blue Rock/Paper/Scissors,
// followed by Red and Blue territory. Returns the number of result lines or
// RPSFISH_ERROR_INVALID_POSITION.
int32_t rpsfish_analyze(
    int32_t mode,
    int32_t side_to_move,
    int32_t ply,
    uint64_t red_rock_low,
    uint64_t red_rock_high,
    uint64_t red_paper_low,
    uint64_t red_paper_high,
    uint64_t red_scissors_low,
    uint64_t red_scissors_high,
    uint64_t blue_rock_low,
    uint64_t blue_rock_high,
    uint64_t blue_paper_low,
    uint64_t blue_paper_high,
    uint64_t blue_scissors_low,
    uint64_t blue_scissors_high,
    uint64_t red_territory_low,
    uint64_t red_territory_high,
    uint64_t blue_territory_low,
    uint64_t blue_territory_high,
    int32_t max_depth,
    int32_t max_nodes,
    int32_t max_time_ms,
    int32_t variations);

// iOS and Android search state is process-global and synchronized. Use one
// serial background queue for each call sequence, and never run analysis on the
// UI thread.
void rpsfish_search_clear(void);
void rpsfish_history_clear(void);
int32_t rpsfish_history_len(void);
int32_t rpsfish_history_truncate(int32_t length);
int32_t rpsfish_history_push(
    int32_t mode,
    int32_t side_to_move,
    int32_t ply,
    uint64_t red_rock_low,
    uint64_t red_rock_high,
    uint64_t red_paper_low,
    uint64_t red_paper_high,
    uint64_t red_scissors_low,
    uint64_t red_scissors_high,
    uint64_t blue_rock_low,
    uint64_t blue_rock_high,
    uint64_t blue_paper_low,
    uint64_t blue_paper_high,
    uint64_t blue_scissors_low,
    uint64_t blue_scissors_high,
    uint64_t red_territory_low,
    uint64_t red_territory_high,
    uint64_t blue_territory_low,
    uint64_t blue_territory_high);

void rpsfish_root_filter_clear(void);
int32_t rpsfish_root_filter_push(int32_t from, int32_t to);

int32_t rpsfish_analysis_count(void);
int32_t rpsfish_analysis_score(void);
int32_t rpsfish_analysis_depth(void);
int32_t rpsfish_analysis_selective_depth(void);
int32_t rpsfish_analysis_confidence(void);
int32_t rpsfish_analysis_nodes(void);
int32_t rpsfish_analysis_elapsed_ms(void);
RPSFishStopReason rpsfish_analysis_stop_reason(void);

int32_t rpsfish_analysis_from(int32_t line_index);
int32_t rpsfish_analysis_to(int32_t line_index);
int32_t rpsfish_analysis_line_score(int32_t line_index);
int32_t rpsfish_analysis_pv_length(int32_t line_index);
int32_t rpsfish_analysis_pv_from(int32_t line_index, int32_t ply);
int32_t rpsfish_analysis_pv_to(int32_t line_index, int32_t ply);

#ifdef __cplusplus
} // extern "C"
#endif

#endif // RPSFISH_H
