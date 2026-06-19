"""
LogitsProcessors for document parsing models.

Contains:
- TableInsertionLogitsProcessor: Forces table structure insertion after coordinates
- RepetitionStopProcessor: Detects repetition/hallucination and forces coordinate tokens
"""

import torch
from collections import Counter
from transformers import LogitsProcessor
from typing import Set, List, Tuple


class TableInsertionLogitsProcessor(LogitsProcessor):
    """
    A LogitsProcessor that inserts \begin{tabular} tokens after coordinate pairs
    that mark the START of an object (not the END coordinates).
    
    Pattern: <x_start><y_start>CONTENT<x_end><y_end><class_...>
    
    This processor triggers after the first <x_...><y_...> pair (start coords),
    but not after the second pair (end coords).
    
    Args:
        tokenizer: The tokenizer used for encoding/decoding
        table_prefix: The string to insert (default: "\\begin{tabular}")
    """
    
    def __init__(
        self, 
        tokenizer, 
        table_prefix: str = "\\begin{tabular}"
    ):
        self.tokenizer = tokenizer
        self.table_prefix = table_prefix
        
        # Tokenize the table prefix to get the sequence of tokens to force
        self.table_prefix_ids = tokenizer.encode(
            table_prefix, 
            add_special_tokens=False
        )
        
        # Build sets of token IDs for detection
        self._build_token_sets()
        
        # State tracking for forced insertion
        self._insertion_position = {}  # batch_idx -> position in table_prefix_ids
        self._insertion_active = {}    # batch_idx -> bool
        
        # State tracking for coordinate pairs
        # False = expecting START coordinates (should trigger)
        # True = expecting END coordinates (should NOT trigger)
        self._expecting_end_coords = {}  # batch_idx -> bool
    
    def _build_token_sets(self):
        """Build sets of token IDs for x_, y_, class_, and special tokens."""
        vocab = self.tokenizer.get_vocab()
        
        # Collect all x_ coordinate tokens
        self.x_token_ids: Set[int] = set()
        for token, token_id in vocab.items():
            if token.startswith("<x_") and token.endswith(">"):
                self.x_token_ids.add(token_id)
        
        # Collect all y_ coordinate tokens
        self.y_token_ids: Set[int] = set()
        for token, token_id in vocab.items():
            if token.startswith("<y_") and token.endswith(">"):
                self.y_token_ids.add(token_id)
        
        # Collect all class tokens
        self.class_token_ids: Set[int] = set()
        for token, token_id in vocab.items():
            if token.startswith("<class_") and token.endswith(">"):
                self.class_token_ids.add(token_id)
    
    def _is_xy_pair(self, input_ids: torch.Tensor, batch_idx: int) -> bool:
        """Check if the sequence ends with <x_...><y_...>."""
        seq = input_ids[batch_idx].tolist()
        
        if len(seq) < 2:
            return False
        
        return seq[-1] in self.y_token_ids and seq[-2] in self.x_token_ids
    
    def _last_token_is_class(self, input_ids: torch.Tensor, batch_idx: int) -> bool:
        """Check if the last token is a <class_...> token."""
        seq = input_ids[batch_idx].tolist()
        
        if len(seq) < 1:
            return False
        
        return seq[-1] in self.class_token_ids
    
    def reset(self):
        """Reset the processor state for a new generation."""
        self._insertion_position = {}
        self._insertion_active = {}
        self._expecting_end_coords = {}
    
    def __call__(
        self, 
        input_ids: torch.LongTensor, 
        scores: torch.FloatTensor
    ) -> torch.FloatTensor:
        """
        Process logits to force table prefix insertion when appropriate.
        
        Args:
            input_ids: (batch_size, seq_len) - tokens generated so far
            scores: (batch_size, vocab_size) - logits for next token
            
        Returns:
            Modified scores with forced tokens where appropriate
        """
        batch_size = input_ids.shape[0]
        
        for batch_idx in range(batch_size):
            # Check if we're currently in an insertion sequence
            if self._insertion_active.get(batch_idx, False):
                pos = self._insertion_position[batch_idx]
                
                if pos < len(self.table_prefix_ids):
                    # Force the next token in the sequence
                    forced_token_id = self.table_prefix_ids[pos]
                    scores[batch_idx] = torch.full_like(scores[batch_idx], float('-inf'))
                    scores[batch_idx, forced_token_id] = 0.0
                    
                    # Advance position
                    self._insertion_position[batch_idx] = pos + 1
                else:
                    # Finished inserting, deactivate
                    self._insertion_active[batch_idx] = False
                continue
            
            # Check if we just saw a <class_...> token - reset state to expect START coords
            if self._last_token_is_class(input_ids, batch_idx):
                self._expecting_end_coords[batch_idx] = False
            
            # Check if we have an <x_...><y_...> pair
            if self._is_xy_pair(input_ids, batch_idx):
                expecting_end = self._expecting_end_coords.get(batch_idx, False)
                
                if not expecting_end:
                    # This is START coordinates - trigger insertion!
                    self._insertion_active[batch_idx] = True
                    self._insertion_position[batch_idx] = 0
                    
                    # After START coords, we expect END coords next
                    self._expecting_end_coords[batch_idx] = True
                    
                    # Force the first token
                    if len(self.table_prefix_ids) > 0:
                        forced_token_id = self.table_prefix_ids[0]
                        scores[batch_idx] = torch.full_like(scores[batch_idx], float('-inf'))
                        scores[batch_idx, forced_token_id] = 0.0
                        self._insertion_position[batch_idx] = 1
                else:
                    # This is END coordinates - don't trigger, but reset for next object
                    # The <class_...> token will come next, which resets expecting_end_coords
                    pass
        
        return scores


class RepetitionStopProcessor(LogitsProcessor):
    """
    A LogitsProcessor that detects CONSECUTIVE repetition/hallucination in generated 
    content and forces the model to output <x_...> tokens to close the current object.
    
    This detects patterns like:
    - <x_0.0><x_0.0><x_0.0>... (same token repeating consecutively)
    - "ABC ABC ABC ABC..." (same phrase repeating consecutively)
    
    But NOT patterns like:
    - "& 1 & 2 & 3 & 4..." (delimiters with different content between them)
    
    When consecutive repetition exceeds the threshold, the processor forces an <x_...> 
    token to start coordinates, effectively ending the current content. After triggering, 
    it enters a cooldown until a <class_...> token is seen (indicating the object was 
    closed properly).
    
    Args:
        tokenizer: The tokenizer used for encoding/decoding
        max_repetitions: Max consecutive repetitions before forcing stop (default: 10)
        ngram_sizes: List of n-gram sizes to check for repetition (default: [1, 2, 3, 4, 5])
        window_size: Size of the sliding window to check for repetitions (default: 2 * max_ngram * (max_repetitions + 1))
    """
    
    def __init__(
        self,
        tokenizer,
        max_repetitions: int = 10,
        ngram_sizes: List[int] = None,
        window_size: int | None = None
    ):
        self.tokenizer = tokenizer
        self.max_repetitions = max_repetitions
        # Check various n-gram sizes for consecutive repetition
        # n=1 catches single token repetition like <x_0.0><x_0.0><x_0.0>...
        # n=2,3,4,5 catch phrase repetition like "ABC ABC ABC..."
        self.ngram_sizes = ngram_sizes if ngram_sizes is not None else [1, 2, 3, 4, 5]
        max_ngram = max((n for n in self.ngram_sizes if n > 0), default=1)
        default_window = 2 * max_ngram * (self.max_repetitions + 1)
        self.window_size = int(window_size) if window_size is not None else int(default_window)
        
        # Build set of token IDs
        self._build_token_sets()
        
        # State tracking - once triggered, stay in cooldown until we see <class_...>
        self._in_cooldown = {}  # batch_idx -> bool
        # Track the start of the "current object segment" (tokens after the last <class_...>)
        # We only look for repetition inside this segment to avoid old repetition re-triggering immediately.
        self._segment_start = {}  # batch_idx -> int (seq index)
    
    def _build_token_sets(self):
        """Build sets of token IDs for detection."""
        vocab = self.tokenizer.get_vocab()
        
        # Collect all x_ coordinate tokens
        self.x_token_ids: Set[int] = set()
        self.x_token_list: List[int] = []
        for token, token_id in vocab.items():
            if token.startswith("<x_") and token.endswith(">"):
                self.x_token_ids.add(token_id)
                self.x_token_list.append(token_id)
        
        # Sort and pick a middle x token as default
        self.x_token_list.sort()
        self.default_x_token = self.x_token_list[len(self.x_token_list) // 2] if self.x_token_list else None
        
        # Collect all class tokens (used to reset cooldown)
        self.class_token_ids: Set[int] = set()
        for token, token_id in vocab.items():
            if token.startswith("<class_") and token.endswith(">"):
                self.class_token_ids.add(token_id)
    
    def _count_consecutive_repetitions(self, seq: List[int], n: int) -> int:
        """
        Count the maximum number of times an n-gram repeats CONSECUTIVELY.
        
        For example, if seq contains "ABC ABC ABC ABC DEF ABC":
        - The n-gram "ABC" appears 4 times consecutively at the start
        - Returns 4 (the max consecutive count)
        
        Returns:
            Maximum consecutive repetition count for any n-gram
        """
        if len(seq) < n:
            return 0
        
        max_consecutive = 1
        current_consecutive = 1
        
        # Slide through the sequence comparing adjacent n-grams
        prev_ngram = tuple(seq[0:n])
        
        i = n
        while i <= len(seq) - n:
            current_ngram = tuple(seq[i:i + n])
            
            if current_ngram == prev_ngram:
                current_consecutive += 1
                max_consecutive = max(max_consecutive, current_consecutive)
                i += n  # Skip by n to check the next occurrence
            else:
                current_consecutive = 1
                prev_ngram = current_ngram
                i += 1  # Move by 1 to find new patterns
        
        return max_consecutive
    
    def _has_excessive_repetition(self, seq: List[int]) -> bool:
        """
        Check if the sequence has excessive CONSECUTIVE repetition of any n-gram.
        
        This detects hallucination patterns like:
        - <x_0.0><x_0.0><x_0.0><x_0.0>... (same token repeating)
        - "ABC ABC ABC ABC..." (same phrase repeating)
        
        But NOT patterns like:
        - "& 1 & 2 & 3 & 4..." (delimiters with different content)
        
        Returns:
            True if any n-gram repeats consecutively more than max_repetitions times
        """
        # Only check the recent window
        check_seq = seq[-self.window_size:] if len(seq) > self.window_size else seq
        
        for n in self.ngram_sizes:
            consecutive_count = self._count_consecutive_repetitions(check_seq, n)
            if consecutive_count > self.max_repetitions:
                return True
        
        return False
    
    def reset(self):
        """Reset the processor state for a new generation."""
        self._in_cooldown = {}
        self._segment_start = {}
    
    def __call__(
        self,
        input_ids: torch.LongTensor,
        scores: torch.FloatTensor
    ) -> torch.FloatTensor:
        """
        Process logits to force <x_...> token when repetition is detected.
        
        Args:
            input_ids: (batch_size, seq_len) - tokens generated so far
            scores: (batch_size, vocab_size) - logits for next token
            
        Returns:
            Modified scores with forced <x_...> tokens where repetition detected
        """
        batch_size = input_ids.shape[0]
        
        for batch_idx in range(batch_size):
            seq = input_ids[batch_idx].tolist()
            seq_len = len(seq)
            
            # Check if we just saw a class token - reset cooldown and start a new segment
            if seq_len > 0 and seq[-1] in self.class_token_ids:
                self._in_cooldown[batch_idx] = False
                self._segment_start[batch_idx] = seq_len
            
            # If in cooldown, don't check for repetition (let model generate naturally)
            if self._in_cooldown.get(batch_idx, False):
                continue
            
            # Check if repetition threshold is exceeded
            segment_start = self._segment_start.get(batch_idx, 0)
            segment_seq = seq[segment_start:]
            if self._has_excessive_repetition(segment_seq):
                # Enter cooldown to avoid repeated triggering
                self._in_cooldown[batch_idx] = True
                
                # Force one of the <x_...> tokens to start closing the object
                # Keep the model's original preferences for which x_ coordinate to use
                original_scores = scores[batch_idx].clone()
                scores[batch_idx] = torch.full_like(scores[batch_idx], float('-inf'))
                
                # Restore original logits for x_ tokens only
                for x_token_id in self.x_token_ids:
                    scores[batch_idx, x_token_id] = original_scores[x_token_id]
        
        return scores
