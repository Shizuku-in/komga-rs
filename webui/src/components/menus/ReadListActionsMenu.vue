<template>
  <div>
    <v-menu location="bottom" v-if="isAdmin" v-model="menuState">
      <template v-slot:activator="{ props }">
        <v-btn icon v-bind="props" @click.prevent="">
          <v-icon>mdi-dots-vertical</v-icon>
        </v-btn>
      </template>
      <v-list density="compact">
        <v-list-item @click="promptDeleteReadList"
                     class="list-danger">
          <v-list-item-title>{{ $t('menu.delete') }}</v-list-item-title>
        </v-list-item>
      </v-list>
    </v-menu>
  </div>
</template>
<script lang="ts">
import {defineComponent} from 'vue'
import {ReadListDto} from '@/types/komga-readlists'

export default defineComponent({
  name: 'ReadListActionsMenu',
  data: function () {
    return {
      menuState: false,
    }
  },
  props: {
    readList: {
      type: Object as () => ReadListDto,
      required: true,
    },
    menu: {
      type: Boolean,
      default: false,
    },
  },
  watch: {
    menuState (val) {
      this.$emit('update:menu', val)
    },
  },
  computed: {
    isAdmin (): boolean {
      return this.$store.getters.meAdmin
    },
  },
  methods: {
    promptDeleteReadList () {
      this.$store.dispatch('dialogDeleteReadList', this.readList)
    },
  },
})
</script>
<style scoped>
@import "../../styles/list-warning.css";
</style>
