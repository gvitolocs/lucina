# sourced by gen.sh: cut the functions the HTC oracle runs out of htc.c
{
  for f in ath10k_htc_process_credit_report ath10k_htc_process_lookahead ath10k_htc_process_lookahead_bundle ath10k_htc_process_trailer; do
    fn "$K/htc.c" $f
    echo
  done
} > "$B/htc_funcs.inc"
